//! One bounded broadcast channel per run with a live log subscriber.
//!
//! # This map is per **process**, and since Task 16c it has a publisher
//!
//! There is one of these per replica and nothing replicates between them, so a
//! line reaches only the subscribers attached to the process that published it.
//! That was always true and was never *reachable*: until Task 16c gave
//! `domain::service::ingest` a driver, nothing published at all.
//!
//! **Today that costs nothing, and stating otherwise would describe a
//! deployment nobody has.** The publisher is `domain::service::watch`'s observer
//! task, started by the dispatcher tick, and the tick runs under
//! `infra::leader`'s role — but `NoopLeaderElector` is the only
//! `impl LeaderElector` in this crate and its `run_role` calls the work
//! unconditionally. So *every* replica ticks, attaches, and publishes into its
//! **own** broadcaster, which is the same one its own router subscribes to
//! (`gear::LogWiring`). There is no replica that is not also the dispatcher, and
//! a subscriber routed anywhere gets lines.
//!
//! **Once a real elector is deployed, this becomes the visible constraint on the
//! whole feature.** Only the leader would tick, so only the leader's map would
//! ever receive a line, while `GET /qa/v1/runs/{id}/logs` is served by whichever
//! replica the load balancer picked: `subscribe` succeeds, the channel is
//! created on demand, and nothing is ever published into it — a **200 and
//! silence**, indistinguishable from a run that has produced no output yet.
//!
//! Written in the conditional deliberately. The first draft of this paragraph
//! stated it in the present tense, which is the framing the plan's own shape
//! section forbids — *"do not describe the gate as if it currently gates
//! anything"* — and the plan's matching bullet has been retracted for the same
//! error. The fix, when it is needed, is a cross-process fan-out (a broker
//! topic, or a durable copy every replica can read), not a change to the
//! watcher or the route.
//!
//! **Corrected 2026-08-31.** This read "or the log archiving feature
//! decouples", pointing at the execution slice, which is about running the
//! workloads and not about log durability; and the decoupling itself has since
//! landed — the durable copy is `qa_run_logs`
//! (`infra::logs::RunLogArchive`). It does not
//! close the gap this paragraph is about, because a *live* stream still only
//! reaches subscribers on the publishing replica; what it closes is the
//! finished-run half.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::{Receiver, Sender, channel};
use uuid::Uuid;

/// Lines a subscriber may fall behind by before it is told it missed some.
///
/// A number rather than "unbounded" is the whole point of this module: an
/// unbounded channel with one slow SSE client is a per-run memory leak that
/// grows for as long as the run produces output, and an 8-hour run
/// (`cpt-cf-qa-nfr-run-duration`) produces a lot of it. The source system never
/// faced the question because it polls a fixed-size log per client rather than
/// buffering (`../testrunner/manager/src/routes/runs.rs:1477-1527`).
///
/// **256 is a judgement, not a ported constant**, and it is stated as one: the
/// source system has no equivalent number to read. It is chosen so that a
/// subscriber which stalls for one dispatcher tick (15 s by default) at a
/// plausible ~15 lines/second still loses nothing.
///
/// # What this bounds, and what it does not
///
/// **It bounds a line count, not a byte count**, and a previous revision of this
/// comment claimed the second: *"a run's worst-case resident buffer stays a few
/// tens of kilobytes per subscribed run"*. That is false and was falsified by
/// execution — a probe at capacity 4 retained 4,000,000 bytes by publishing four
/// 1 MB lines. At 256 the resident buffer is `256 x` the longest line the
/// executor emits, so a runner that emits a 2 MB line holds 512 MB per subscribed
/// run.
///
/// The quantity is therefore **unbounded in bytes by this constant alone**, and
/// what actually bounds it is the executor adapter: capping it *here* was
/// considered and declined — truncating an operator's log line silently is the
/// same class of harm as truncating a status, and this module has no way to say
/// "the rest of this line is in the archived log". **The cap belongs to
/// whoever writes the adapter**, and it is recorded as an obligation rather
/// than implied: an adapter that forwards unbounded lines makes this constant
/// a multiplier on an unbounded quantity.
///
/// **Task 15 (review finding #30) is one adapter taking that obligation up.**
/// `infra::executor::argo::watch::handle_line` now truncates with
/// `domain::repos::sanitize_line_for_archive` before a line ever reaches
/// `ExecutionEvent::Log`, so the argo adapter's own contribution to this
/// buffer is bounded. This is not true of every adapter: `MockRunExecutor`
/// does not truncate, and neither would the HTTP-push producer this
/// module's own doc anticipates landing one day (`domain::service::ingest`'s
/// `apply` is `pub`) unless it is written to. The obligation above is still
/// live for any adapter that has not taken it up.
///
/// Nothing tests the sizing, because "how far behind does a real SSE client fall"
/// is not a question this crate can answer; what *is* tested is that overflowing
/// the count produces [`gap_marker`] rather than silence.
pub const DEFAULT_LOG_CHANNEL_CAPACITY: usize = 256;

/// Lines of a run's output kept after the fact, so a finished run still has a log.
///
/// # Why anything is retained at all, which was a deliberate "no" until 2026-08-27
///
/// This type's own doc used to say — and the SSE endpoint implemented —
/// *"lines emitted before anyone subscribes are therefore dropped … the archived
/// log (`Run::log_storage_ref`, feature 2.7) is what serves a consumer that
/// arrives late; this is the live tap, not the record."* Feature 2.7 did not
/// exist yet: nothing in this gear wrote a durable copy, so the honest
/// description of the shipped behaviour was **a run's output is unreadable
/// unless somebody had the log pane open while it was printed**, and for a run
/// whose pod prints its whole output in three seconds (measured: run
/// `94978978-fa28-4650-a14d-2ce8f72dff49`, 179 lines between 19:33:09 and
/// 19:33:12 after four minutes of `Pending`) that is nobody.
///
/// **A durable archive now exists**, in `qa_run_logs`
/// (`infra::logs::RunLogArchive`, Task 2-5), and `api::rest::handlers::runs`
/// reads it back through `RunsService::archived_log` before ever falling back
/// to this tail. `qa_runs.log_storage_ref` stays unwritten regardless — design
/// D-RLP-6: it is published in `RunDto` shaped like a fetchable URI, so an
/// internal table reference (`db:qa_run_logs/{run_id}`) does not belong in it.
/// That is a decision about one column, not a statement that no archive
/// exists.
///
/// So this tail is no longer the only copy of a finished run's output — it is
/// a **cache in front of the archive**: in-process and bounded, so it does not
/// survive a restart or cross replicas, and it still drops the *head* of a
/// long log rather than storing it. What it now buys is the runs the archive
/// has no row for — every run that finished before `qa_run_logs` existed — and
/// nothing more; `Self::replay`'s callers reach for it only when the archive
/// answers `None`.
///
/// # The three bounds, and why a line count alone will not do
///
/// [`DEFAULT_LOG_CHANNEL_CAPACITY`]'s doc records the mistake this avoids: a line
/// count *is not* a memory bound, because nothing caps a line's length — a probe
/// at capacity 4 held 4 MB by publishing four 1 MB lines. So the per-run tail is
/// bounded by **both** a line count and a byte total, and the number of retained
/// runs is bounded too, because a terminal run's tail is kept precisely when
/// nothing is watching it and would otherwise never be released.
///
/// Worst case resident: `MAX_RETAINED_RUNS x MAX_RETAINED_BYTES_PER_RUN` plus one
/// over-long line per run, i.e. ~16 MB plus slack. That is a real cost and it is
/// stated rather than left to be discovered.
pub const MAX_RETAINED_LINES_PER_RUN: usize = 5_000;

/// The byte half of the per-run bound. See [`MAX_RETAINED_LINES_PER_RUN`].
///
/// One line longer than this is still retained whole — evicting it would leave a
/// run with a visibly empty log and no explanation, and the marker line says what
/// was dropped only when something else remains to read.
pub const MAX_RETAINED_BYTES_PER_RUN: usize = 512 * 1024;

/// Runs whose tail is kept at once, least-recently-written evicted first.
///
/// The bound that makes retention safe: without it the map grows one entry per
/// run the control plane ever observed and nothing ever removes it, which is the
/// exact leak `publish`-mints-no-channel was written to avoid. An entry with no
/// live channel is preferred for eviction over one with a subscriber, so an
/// operator watching a run does not lose its head to another run finishing.
pub const MAX_RETAINED_RUNS: usize = 32;

/// The line a late reader gets in place of the head of a log too long to retain.
///
/// Distinct wording from [`gap_marker`], which means something else — that a
/// *live* subscriber fell behind. Same caveat about a runner printing this text
/// verbatim, and the same remedy.
#[must_use]
pub fn truncation_marker(dropped: u64) -> String {
    format!(
        "[qa-runs] log truncated: the first {dropped} line(s) of this run are no longer retained"
    )
}

/// Live subscribers one run may have at once.
///
/// **A bound on connections, added 2026-08-15**, because the one this module
/// claimed did not exist. Item 1 below used to say the map's size was bounded
/// "by the HTTP layer's connection limit"; there is no such limit - the inbound
/// stack (`libs/toolkit/src/runtime/oop_serve.rs`) applies auth, a drain guard
/// and canonical-error middleware, and no concurrency or timeout layer. A
/// caller could authenticate, launch one run, and hold an unbounded number of
/// SSE connections against it for `MAX_STREAM_DURATION`.
///
/// Sixteen is a judgement, not a ported constant: a run is watched by a handful
/// of operators and a CI job, and a seventeenth simultaneous viewer of one run
/// is far more likely to be a leak than a person.
///
/// # What it does not bound
///
/// The **total** across runs. Every connection still needs a run the caller can
/// read under the PEP, but a tenant may have many runs, so a determined caller
/// can hold `16 x (their runs)` connections. Bounding that needs a per-subject
/// or global limit, which belongs to the HTTP layer rather than here - this
/// type has no `SecurityContext` and cannot see a subject.
pub const MAX_SUBSCRIBERS_PER_RUN: usize = 16;

/// The line a subscriber receives in place of the `skipped` lines it missed.
///
/// ASCII only — `clippy::non_ascii_literal` is denied workspace-wide, so no
/// ellipsis glyph — and prefixed.
///
/// **The prefix is a convention, not a guarantee**: published lines are the
/// execution plane's bytes verbatim, so a runner printing this exact text is
/// indistinguishable from a real gap. Same caveat as
/// `domain::repos::log_line`'s truncation marker, and the same remedy if it ever
/// matters — a distinct event type rather than a magic string.
///
/// **Explicit rather than silent, which is the requirement.** `tokio`'s
/// broadcast receiver reports a lag as [`RecvError::Lagged`] and then resumes
/// from the oldest retained value; a caller that swallowed it would hand an
/// operator a log with an invisible hole in the middle, which is worse than a
/// visibly truncated one because nothing on screen says to go look at the
/// archived log instead.
#[must_use]
pub fn gap_marker(skipped: u64) -> String {
    format!("[qa-runs] log gap: {skipped} line(s) were dropped because this subscriber fell behind")
}

/// A live subscriber's end of one run's log stream.
///
/// Not a `Stream`, for the reason
/// [`ExecutionStream`](crate::domain::ports::run_executor::ExecutionStream)
/// gives about itself: the contract ("ends when the run's channel is reaped",
/// "a lag becomes a [`gap_marker`] line") is a property of this type and is
/// documented once here, rather than of a boxed trait object each consumer
/// re-describes.
///
/// # It prunes its run's channel when it is the last one to go
///
/// **This is what bounds [`RunLogBroadcaster`]'s map**, and it was added after a
/// probe subscribed to 1,000 nonexistent run ids, dropped every subscription, and
/// left `active_channels() == 1000` — permanently, because nothing published to
/// them so the prune-on-failed-publish never fired and nothing reaped them.
///
#[derive(Debug)]
pub struct LogSubscription {
    rx: Receiver<String>,
    /// **Declared after [`Self::rx`], and the order is the whole mechanism.**
    /// Struct fields drop in declaration order, and a `Drop` impl on the *outer*
    /// type runs *before* any field is dropped — so a `drop` written on
    /// `LogSubscription` itself would still see its own `Receiver` alive and read
    /// `receiver_count() == 1`, never pruning. Verified by execution: that is
    /// exactly what the first version of this type did, and the 1,000-subscription
    /// test caught it.
    ///
    /// Nesting the hook in a field that is declared last means `rx` is gone by the
    /// time it runs, so the count is the truth and `== 0` is exact rather than an
    /// off-by-one guess like `<= 1`.
    _prune: PruneOnDrop,
}

/// Releases a run's channel once the last subscription to it goes.
///
/// A `Weak` and not an `Arc`: a subscription must not keep the broadcaster alive,
/// and a broadcaster already dropped has no map left to prune.
#[derive(Debug)]
struct PruneOnDrop {
    run_id: Uuid,
    owner: Weak<RunLogBroadcaster>,
}

impl Drop for PruneOnDrop {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.prune_if_idle(self.run_id);
        }
    }
}

impl LogSubscription {
    /// The next log line, or `None` once the run's channel has been reaped and
    /// every buffered line has been drained.
    ///
    /// A lag is **not** an error to the caller: it is delivered as one
    /// [`gap_marker`] line and the stream continues. `None` therefore means
    /// exactly one thing — there will be no more lines — which is what an SSE
    /// handler needs in order to decide whether to close the response.
    pub async fn recv(&mut self) -> Option<String> {
        match self.rx.recv().await {
            Ok(line) => Some(line),
            Err(RecvError::Lagged(skipped)) => Some(gap_marker(skipped)),
            Err(RecvError::Closed) => None,
        }
    }
}

/// Per-run bounded fan-out of live log lines.
///
/// # Lifetime of a run's channel
///
/// A channel is created by the **first subscriber** and destroyed by
/// [`reap`](Self::reap), which `domain::service::ingest` calls when the run
/// reaches a terminal state.
///
/// **Publishing does not create one, and that is deliberate.** A `broadcast`
/// channel delivers only to receivers that already exist, so a channel minted by
/// a publish would have no reader and would hold a slot in this map for every
/// run the control plane ever observed — a leak keyed by run id rather than by
/// subscriber.
///
/// # A publish does, since 2026-08-27, create a **retained tail**
///
/// The paragraph above used to continue: *"Lines emitted before anyone subscribes
/// are therefore dropped … The archived log (`Run::log_storage_ref`, feature 2.7)
/// is what serves a consumer that arrives late; this is the live tap, not the
/// record."* At the time, feature 2.7 was only a plan: nothing in this gear
/// wrote a durable copy yet, so the first half was not a division of labour, it
/// was the whole behaviour: **a run's output was unreadable unless someone had
/// the log pane open while it printed.**
///
/// **A durable archive now exists**, in `qa_run_logs`
/// (`infra::logs::RunLogArchive`, Task 2-5). `qa_runs.log_storage_ref` is still
/// unwritten - design D-RLP-6 keeps it that way deliberately, because it is
/// published in `RunDto` shaped like a fetchable URI and an internal table
/// reference does not belong there - but the archive itself is real.
///
/// So the live channel is still minted only by a subscriber, and the *retained
/// tail* is minted by a publish and bounded three ways
/// ([`MAX_RETAINED_LINES_PER_RUN`], [`MAX_RETAINED_BYTES_PER_RUN`],
/// [`MAX_RETAINED_RUNS`]). It is now a **cache in front of the archive**
/// rather than the only copy: [`Self::replay`] serves a finished run only when
/// `RunsService::archived_log` has no row for it, and
/// [`Self::subscribe_with_replay`] still serves a live one, which has no
/// archived text yet to prefer.
///
/// # What bounds this map, in three layers
///
/// **The map is keyed by run id and every entry is minted by a caller, so a
/// bound is a safety property rather than tidiness.** A probe subscribed to 1,000
/// attacker-chosen UUIDs, dropped every subscription, and left 1,000 permanent
/// entries: nothing published to them, so the publish-side prune never fired, and
/// they were not runs, so nothing reaped them.
///
/// 1. **[`LogSubscription`] prunes on drop** when it was the last receiver. This
///    is the load-bearing bound on *entries*: it makes the map's size at most
///    "runs with a live subscriber". The probe above now ends at `0`.
///
///    **Corrected 2026-08-15.** This said that size was in turn bounded by "the
///    HTTP layer's connection limit". There is no such limit: the inbound stack
///    applies auth, a drain guard and canonical-error middleware, and nothing
///    that caps concurrency or times a request out. What bounds subscribers per
///    run is now [`MAX_SUBSCRIBERS_PER_RUN`], added here; what still bounds the
///    total across runs is nothing, and that constant says so.
/// 2. **A failed publish prunes.** Now a *race-window* backstop rather than an
///    independent bound, and the narrowing is worth stating precisely: with (1)
///    in place a receiver count can only reach zero through a drop that then
///    prunes, so the only way a publish finds no receivers is if it takes the map
///    lock in the window between the last `Receiver` being dropped and
///    `prune_if_idle` acquiring that lock. Kept because it costs nothing and
///    closes that window; **not** a second general bound, and an earlier revision
///    of this list claimed it covered a `mem::forget`ten receiver, which is false
///    — a forgotten `Receiver` keeps the count above zero and the send succeeds.
/// 3. **[`reap`](Self::reap) ends live subscriptions** when the run reaches a
///    terminal state — which is what makes `LogSubscription::recv` answer `None`,
///    and therefore what lets an SSE handler close its response. Without it the
///    map stayed bounded but every cancelled run's SSE connection hung open
///    forever.
///
///    **The callers are named, not counted, in
///    `domain::service::LogFanout::reap`.** An earlier revision enumerated them
///    here and was wrong at both ends — it said "five" over six items, omitted a
///    seventh, and opened with "every path in this gear", which was false because
///    `service::launch` had no field to call through at all. One list, in the
///    trait, is why that cannot happen twice.
///
/// **Authorizing the subscription is the endpoint's job**, and it does it:
/// `api::rest::handlers::runs::stream_run_logs` reads the run under the
/// caller's own `qa.run`/`get` scope *before* calling
/// [`subscribe`](Self::subscribe), so a run that does not exist or is another
/// tenant's answers 404 with no channel minted. This type performs no
/// repository access and could not do it. What remains bounded only by
/// connections is a caller subscribing to **their own** runs, which
/// [`MAX_SUBSCRIBERS_PER_RUN`] caps per run and nothing caps in total.
#[derive(Debug)]
pub struct RunLogBroadcaster {
    /// **One mutex over both maps, and that is load-bearing rather than tidy.**
    /// [`Self::subscribe_with_replay`] has to hand back a snapshot of the retained
    /// tail *and* a live subscription with no line falling between them: two
    /// locks would either lose a line published in the gap (snapshot first) or
    /// deliver it twice (subscribe first), and a log viewer showing a duplicated
    /// line is indistinguishable from a test that printed twice.
    inner: Mutex<Inner>,
    capacity: usize,
}

/// The live channels, the retained tails, and the write counter that orders them.
#[derive(Debug, Default)]
struct Inner {
    channels: HashMap<Uuid, Sender<String>>,
    retained: HashMap<Uuid, RetainedLog>,
    /// Monotonic per-publish counter, used as the LRU key for
    /// [`MAX_RETAINED_RUNS`]. A counter and not a timestamp: two publishes in the
    /// same clock tick must still order, and this map's eviction must not depend
    /// on a clock that can go backwards.
    writes: u64,
}

/// One run's bounded retained tail.
#[derive(Debug, Default)]
struct RetainedLog {
    lines: VecDeque<String>,
    /// Sum of `lines`' lengths, maintained incrementally: recomputing it per
    /// publish is O(retained lines) on the run path, which is the one path that
    /// must not become quadratic in a chatty run's output.
    bytes: usize,
    /// How many lines were evicted from the front. Surfaced as
    /// [`truncation_marker`], because a silently truncated log is the failure this
    /// module has already been burned by once (see [`gap_marker`]).
    dropped: u64,
    writes_at: u64,
}

impl Inner {
    /// Append to `run_id`'s tail, then enforce all three bounds.
    fn retain(&mut self, run_id: Uuid, line: &str) {
        self.writes = self.writes.saturating_add(1);
        let writes = self.writes;
        let log = self.retained.entry(run_id).or_default();
        log.writes_at = writes;
        log.bytes = log.bytes.saturating_add(line.len());
        log.lines.push_back(line.to_owned());
        while log.lines.len() > MAX_RETAINED_LINES_PER_RUN
            || (log.bytes > MAX_RETAINED_BYTES_PER_RUN && log.lines.len() > 1)
        {
            let Some(evicted) = log.lines.pop_front() else {
                break;
            };
            log.bytes = log.bytes.saturating_sub(evicted.len());
            log.dropped = log.dropped.saturating_add(1);
        }
        self.evict_retained_runs(run_id);
    }

    /// Hold [`MAX_RETAINED_RUNS`], never evicting `keep`.
    ///
    /// Two passes on purpose: a run nobody is watching goes before a run somebody
    /// is, so an operator with the log pane open does not lose the head of the
    /// log they are reading because an unrelated run finished.
    fn evict_retained_runs(&mut self, keep: Uuid) {
        while self.retained.len() > MAX_RETAINED_RUNS {
            let victim = self
                .oldest_retained(keep, true)
                .or_else(|| self.oldest_retained(keep, false));
            match victim {
                Some(id) => {
                    self.retained.remove(&id);
                }
                // Every retained run is `keep`, which cannot happen while the
                // length is above a cap of 1 or more — but a `break` rather than
                // an `unreachable!()`, because the cost of being wrong is a log
                // that is a little too large and not a panic on the run path.
                None => break,
            }
        }
    }

    fn oldest_retained(&self, keep: Uuid, unwatched_only: bool) -> Option<Uuid> {
        self.retained
            .iter()
            .filter(|(id, _)| **id != keep)
            .filter(|(id, _)| !unwatched_only || !self.channels.contains_key(id))
            .min_by_key(|(_, log)| log.writes_at)
            .map(|(id, _)| *id)
    }

    /// `run_id`'s retained tail, with a [`truncation_marker`] first when its head
    /// was evicted.
    fn replay(&self, run_id: Uuid) -> Vec<String> {
        let Some(log) = self.retained.get(&run_id) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(log.lines.len() + 1);
        if log.dropped > 0 {
            out.push(truncation_marker(log.dropped));
        }
        out.extend(log.lines.iter().cloned());
        out
    }
}

impl Default for RunLogBroadcaster {
    fn default() -> Self {
        Self::new(DEFAULT_LOG_CHANNEL_CAPACITY)
    }
}

impl RunLogBroadcaster {
    /// `capacity` is clamped to at least 1, matching
    /// [`ExecutionStream::channel`](crate::domain::ports::run_executor::ExecutionStream::channel):
    /// the only alternative is a panic on a value that is never meaningful.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            capacity: capacity.max(1),
        }
    }

    /// Subscribe to `run_id`'s live log, creating its channel on first use.
    ///
    /// The subscription only ever carries lines published **after** it was
    /// created, and this method hands back nothing else — which is why
    /// [`Self::subscribe_with_replay`], not this, is what the SSE route calls
    /// for a live run.
    ///
    /// **Corrected 2026-08-31.** This said a late subscriber "is served by the
    /// archived log rather than by a replay buffer", which contradicted the
    /// type's own doc above and was wrong twice over: a *live* run has no
    /// archived text to be served from — the archive is written by flushes that
    /// lag behind the stream, and the design deliberately leaves
    /// archive-replay-to-a-late-joiner out of scope because the archive and the
    /// pending buffer overlap at the last flush point — and the retained tail
    /// this type maintains, reached through `subscribe_with_replay`, is exactly
    /// the replay buffer that sentence denied existed.
    ///
    /// **Takes `&Arc<Self>` rather than `&self`**, which is the whole mechanism
    /// behind bound (1) in this type's doc: the returned [`LogSubscription`] has
    /// to be able to reach back and prune the entry it created, and a `&self`
    /// receiver gives it nothing to hold a `Weak` to. This is not a convenience
    /// signature — a `&self` version could not be written safely.
    ///
    /// **This performs no authorization and cannot.** It is infrastructure with
    /// no repository and no `SecurityContext`; the endpoint that calls it owes
    /// the existence and tenancy check, exactly as `service::ingest::apply` now
    /// makes for the publishing side.
    /// Returns `None` when `run_id` already has [`MAX_SUBSCRIBERS_PER_RUN`]
    /// live subscribers - see that constant for what the cap does and does not
    /// bound. The caller answers a resource-exhausted error; it must not retry
    /// in a loop.
    #[must_use]
    pub fn subscribe(self: &Arc<Self>, run_id: Uuid) -> Option<LogSubscription> {
        let rx = {
            let mut inner = self.lock();
            Self::subscribe_locked(&mut inner, run_id, self.capacity)?
        };
        Some(LogSubscription {
            rx,
            _prune: PruneOnDrop {
                run_id,
                owner: Arc::downgrade(self),
            },
        })
    }

    /// `run_id`'s retained tail **and** a live subscription, taken together.
    ///
    /// This is what the SSE endpoint uses for a run that is still active: the
    /// replay is everything printed before the reader arrived and the subscription
    /// is everything printed after, with no line lost or duplicated between them
    /// because both come out of one lock acquisition. See [`Self::inner`].
    ///
    /// `None` for the same reason [`Self::subscribe`] returns it — the run is at
    /// [`MAX_SUBSCRIBERS_PER_RUN`].
    #[must_use]
    pub fn subscribe_with_replay(
        self: &Arc<Self>,
        run_id: Uuid,
    ) -> Option<(Vec<String>, LogSubscription)> {
        let (replay, rx) = {
            let mut inner = self.lock();
            let replay = inner.replay(run_id);
            let rx = Self::subscribe_locked(&mut inner, run_id, self.capacity)?;
            (replay, rx)
        };
        Some((
            replay,
            LogSubscription {
                rx,
                _prune: PruneOnDrop {
                    run_id,
                    owner: Arc::downgrade(self),
                },
            },
        ))
    }

    /// What a reader gets for a run that will print nothing more.
    ///
    /// Deliberately **not** a subscription: a terminal run has no live channel and
    /// minting one would put an entry in the map for every finished run anyone
    /// looks at, and would consume one of the run's sixteen subscriber slots for a
    /// stream that can never carry a line.
    #[must_use]
    pub fn replay(&self, run_id: Uuid) -> Vec<String> {
        self.lock().replay(run_id)
    }

    fn subscribe_locked(
        inner: &mut Inner,
        run_id: Uuid,
        capacity: usize,
    ) -> Option<Receiver<String>> {
        {
            let sender = inner
                .channels
                .entry(run_id)
                .or_insert_with(|| channel(capacity).0);
            if sender.receiver_count() >= MAX_SUBSCRIBERS_PER_RUN {
                // **No cleanup is needed, and the reason is worth stating.** A
                // refusal implies the count is already at the cap, so the entry
                // cannot be one this call just created - `or_insert_with` mints
                // a sender with zero receivers, and zero is never `>=` the cap.
                // The map therefore never gains an orphan from a refused
                // subscribe.
                //
                // A `receiver_count() == 0` cleanup branch stood here until
                // 2026-08-15, with a comment calling it "the whole point of the
                // cap". It required `0 >= 16` and was dead by construction:
                // deleting it, and putting `unreachable!()` in it, were both
                // green. The property held - by this argument, not by that
                // code.
                return None;
            }
            Some(sender.subscribe())
        }
    }

    /// Fan one line out to `run_id`'s current subscribers.
    ///
    /// Returns how many received it, which is `0` both when nobody is watching
    /// and when the last watcher has just gone. Never blocks and never fails:
    /// the run path must not be able to stall behind a log consumer.
    pub fn publish(&self, run_id: Uuid, line: String) -> usize {
        let mut inner = self.lock();
        // Retained FIRST and unconditionally, which is the whole of the 2026-08-27
        // fix: the live send below reaches whoever is watching *now*, and this is
        // what lets a reader who arrives later — including after the run has
        // finished — see anything at all. See [`MAX_RETAINED_LINES_PER_RUN`].
        inner.retain(run_id, &line);
        let Some(sender) = inner.channels.get(&run_id) else {
            return 0;
        };
        if let Ok(received) = sender.send(line) {
            return received;
        }
        // Every receiver has gone. See the type's doc: `reap` is not guaranteed
        // to run for every run, so this is the second bound on the map's size.
        inner.channels.remove(&run_id);
        0
    }

    /// Drop `run_id`'s **live channel**, ending every live subscription once its
    /// buffered lines are drained. `true` when there was one.
    ///
    /// The retained tail is deliberately left behind. Reaping it too would restore
    /// exactly the defect this retention exists to fix — the run has just reached
    /// a terminal state, which is the moment a human goes looking at its log — and
    /// [`MAX_RETAINED_RUNS`] is what releases it instead.
    pub fn reap(&self, run_id: Uuid) -> bool {
        self.lock().channels.remove(&run_id).is_some()
    }

    /// Drop `run_id`'s retained tail as well. Exists for tests and for a caller
    /// that knows a run's log will never be read again; nothing on the run path
    /// calls it, because "never read again" is not a thing this gear knows.
    pub fn forget(&self, run_id: Uuid) -> bool {
        self.lock().retained.remove(&run_id).is_some()
    }

    /// Remove `run_id`'s channel if nothing is listening to it any more.
    ///
    /// Called by [`LogSubscription`]'s `Drop`. Conditional on the receiver count
    /// rather than unconditional, because two SSE clients can watch one run and
    /// the first to disconnect must not silence the second.
    ///
    /// The count is read under the same lock the removal takes, so a subscriber
    /// arriving concurrently either appears in the count (and the entry survives)
    /// or takes the lock afterwards and re-creates it. The one thing this cannot
    /// do is resurrect a subscription that was created against the removed
    /// sender — that is the same window `reap` already has, and a subscriber in
    /// it sees the stream end, which is exactly what it means.
    fn prune_if_idle(&self, run_id: Uuid) {
        let mut inner = self.lock();
        if let Entry::Occupied(entry) = inner.channels.entry(run_id)
            && entry.get().receiver_count() == 0
        {
            entry.remove();
        }
    }

    /// How many runs currently have a **live** channel. The observable form of
    /// "the map does not grow without bound".
    #[must_use]
    pub fn active_channels(&self) -> usize {
        self.lock().channels.len()
    }

    /// How many runs currently have a retained tail. The observable form of
    /// [`MAX_RETAINED_RUNS`], which is the only thing that bounds retention.
    #[must_use]
    pub fn retained_runs(&self) -> usize {
        self.lock().retained.len()
    }

    /// `PoisonError::into_inner` rather than `unwrap`: `clippy::unwrap_used` is
    /// denied, and [`Inner`] has no invariant a panicking holder could have left
    /// broken — the worst a poisoned lock costs here is a log line, or a retained
    /// tail whose byte total is off by one line's length.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The domain seam, satisfied here because the trait is a domain contract and
/// the live channel handles are infrastructure — no domain signature may name
/// this type directly.
///
/// (It said "declared beside its consumer in the domain layer" until 2026-08-14 —
/// the rationale the trait's move to `domain::service` invalidated, corrected one
/// file over in `infra::logs` and left standing here.)
impl crate::domain::service::LogFanout for RunLogBroadcaster {
    fn publish(&self, run_id: Uuid, line: String) {
        // The receiver count is diagnostic only; a run nobody is watching is the
        // ordinary case, not a failure.
        let _ = RunLogBroadcaster::publish(self, run_id, line);
    }

    fn reap(&self, run_id: Uuid) {
        let _ = RunLogBroadcaster::reap(self, run_id);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const RUN: Uuid = Uuid::from_u128(0x51);
    const OTHER: Uuid = Uuid::from_u128(0x52);

    #[tokio::test]
    async fn a_subscriber_receives_lines_published_after_it_subscribed() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let mut sub = bus
            .subscribe(RUN)
            .expect("under the per-run subscriber cap");
        assert_eq!(bus.publish(RUN, "first".to_owned()), 1);
        assert_eq!(bus.publish(RUN, "second".to_owned()), 1);

        assert_eq!(sub.recv().await.as_deref(), Some("first"));
        assert_eq!(sub.recv().await.as_deref(), Some("second"));
    }

    /// The other half of that guarantee, and it changed on 2026-08-27. A line
    /// published before anyone subscribed reaches no *live* receiver and mints no
    /// channel — but it is no longer **gone**, because that is what made a
    /// finished run's log unreadable. This test's name said "is dropped" and its
    /// body asserted only the channel half, so the behaviour it was read as
    /// pinning was never pinned.
    #[tokio::test]
    async fn a_line_published_with_no_subscriber_mints_no_channel_but_is_retained() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        assert_eq!(bus.publish(RUN, "unheard".to_owned()), 0);
        assert_eq!(
            bus.active_channels(),
            0,
            "a publish must not mint a channel; that would leak one entry per run"
        );
        assert_eq!(
            bus.replay(RUN),
            vec!["unheard".to_owned()],
            "and it must still be readable afterwards, which is the whole fix"
        );

        let mut sub = bus
            .subscribe(RUN)
            .expect("under the per-run subscriber cap");
        assert_eq!(bus.publish(RUN, "heard".to_owned()), 1);
        assert_eq!(sub.recv().await.as_deref(), Some("heard"));
    }

    /// **The defect, at the layer it lives in.** Run
    /// `94978978-fa28-4650-a14d-2ce8f72dff49`'s pod printed 179 lines in three
    /// seconds after four minutes of `Pending`; the adapter followed and ingested
    /// every one of them (its seven collection-error rows are in
    /// `qa_run_test_results`), and the endpoint answered a reader afterwards with
    /// nothing at all. Nobody was subscribed while it printed, and nothing kept
    /// what it printed.
    #[tokio::test]
    async fn a_run_that_nobody_watched_is_still_readable_after_it_is_reaped() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        for line in [
            "collected 25 items / 7 errors",
            "runner: pytest exit status 2",
        ] {
            assert_eq!(
                bus.publish(RUN, line.to_owned()),
                0,
                "no live receiver, which is the case that used to lose everything"
            );
        }
        assert!(!bus.reap(RUN), "there was never a live channel to reap");

        assert_eq!(
            bus.replay(RUN),
            vec![
                "collected 25 items / 7 errors".to_owned(),
                "runner: pytest exit status 2".to_owned(),
            ]
        );
    }

    /// A reader who arrives mid-run gets what it missed and then what follows,
    /// with no line lost or repeated at the join. The publish between the two
    /// `recv`s is what makes the "then follows live" half load-bearing.
    #[tokio::test]
    async fn subscribe_with_replay_hands_back_the_missed_lines_then_follows_live() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        bus.publish(RUN, "before one".to_owned());
        bus.publish(RUN, "before two".to_owned());

        let (replay, mut sub) = bus
            .subscribe_with_replay(RUN)
            .expect("under the per-run subscriber cap");
        assert_eq!(
            replay,
            vec!["before one".to_owned(), "before two".to_owned()]
        );

        assert_eq!(bus.publish(RUN, "after".to_owned()), 1);
        assert_eq!(sub.recv().await.as_deref(), Some("after"));
    }

    /// Reap ends the live stream and keeps the tail; only `forget` drops it.
    #[tokio::test]
    async fn reap_ends_the_stream_and_keeps_the_tail() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let mut sub = bus
            .subscribe(RUN)
            .expect("under the per-run subscriber cap");
        bus.publish(RUN, "output".to_owned());

        assert!(bus.reap(RUN));
        assert_eq!(sub.recv().await.as_deref(), Some("output"));
        assert_eq!(sub.recv().await, None, "the live stream must still end");
        assert_eq!(
            bus.replay(RUN),
            vec!["output".to_owned()],
            "and the tail must survive the reap, or a run is unreadable the \
             instant it finishes - which was the defect"
        );

        assert!(bus.forget(RUN));
        assert!(bus.replay(RUN).is_empty());
        assert!(!bus.forget(RUN), "forgetting twice is a no-op");
    }

    /// The line bound, and that it is *visible*. A silently truncated log is the
    /// failure `gap_marker` already exists for; this is the same rule for the
    /// other kind of loss.
    #[tokio::test]
    async fn the_retained_tail_is_bounded_by_lines_and_says_what_it_dropped() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let published = MAX_RETAINED_LINES_PER_RUN + 3;
        for n in 0..published {
            bus.publish(RUN, format!("line {n}"));
        }

        let replay = bus.replay(RUN);
        assert_eq!(
            replay.len(),
            MAX_RETAINED_LINES_PER_RUN + 1,
            "the cap, plus the marker line"
        );
        assert_eq!(replay[0], truncation_marker(3));
        assert_eq!(
            replay[1], "line 3",
            "the head is what was dropped, so the tail is the newest output"
        );
        assert_eq!(replay[replay.len() - 1], format!("line {}", published - 1));
    }

    /// The byte bound, which is the one a line count cannot give — see
    /// `DEFAULT_LOG_CHANNEL_CAPACITY`'s doc for the probe that proved it.
    #[tokio::test]
    async fn the_retained_tail_is_bounded_by_bytes_not_only_by_lines() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let big = "x".repeat(64 * 1024);
        for _ in 0..32 {
            bus.publish(RUN, big.clone());
        }

        let replay = bus.replay(RUN);
        let retained: usize = replay
            .iter()
            .filter(|line| line.len() == big.len())
            .map(String::len)
            .sum();
        assert!(
            retained <= MAX_RETAINED_BYTES_PER_RUN,
            "32 x 64 KiB is 2 MiB and only {MAX_RETAINED_BYTES_PER_RUN} may be held; \
             retained {retained}"
        );
        assert!(
            replay
                .first()
                .is_some_and(|line| line.starts_with("[qa-runs] log truncated")),
            "and the loss must be visible"
        );
    }

    /// One over-long line is kept whole rather than leaving an empty log.
    #[tokio::test]
    async fn a_single_over_long_line_is_retained_rather_than_leaving_nothing() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let huge = "y".repeat(MAX_RETAINED_BYTES_PER_RUN * 2);
        bus.publish(RUN, huge.clone());
        assert_eq!(bus.replay(RUN), vec![huge]);
    }

    /// The bound that makes retention safe: without it the map grows one entry per
    /// run the control plane ever observed. The watched run must survive, so an
    /// operator reading a log does not lose it because other runs finished.
    #[tokio::test]
    async fn retention_is_bounded_by_run_count_and_prefers_evicting_unwatched_runs() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let watched = Uuid::from_u128(0x9999);
        let _sub = bus
            .subscribe(watched)
            .expect("under the per-run subscriber cap");
        bus.publish(watched, "watched output".to_owned());

        for n in 0..(MAX_RETAINED_RUNS as u128 * 2) {
            bus.publish(Uuid::from_u128(0xA000_0000 + n), format!("noise {n}"));
        }

        assert_eq!(
            bus.retained_runs(),
            MAX_RETAINED_RUNS,
            "the number of retained tails is capped"
        );
        assert_eq!(
            bus.replay(watched),
            vec!["watched output".to_owned()],
            "and the run somebody is actually watching is the last to go"
        );
    }

    /// The retention bounds' **values**, which are the contract: they are the sole
    /// bound on this type's resident memory, and every other test here uses them
    /// symbolically, so widening any of them to any number was green.
    #[test]
    fn the_retention_bounds_are_the_numbers_they_promise() {
        assert_eq!(MAX_RETAINED_LINES_PER_RUN, 5_000);
        assert_eq!(MAX_RETAINED_BYTES_PER_RUN, 512 * 1024);
        assert_eq!(MAX_RETAINED_RUNS, 32);
    }

    /// The bound doing its job. Capacity 2, four lines published before a single
    /// `recv`, so the subscriber has provably fallen behind — and gets a line
    /// saying so rather than a stream that silently resumes two lines later.
    #[tokio::test]
    async fn a_lagging_subscriber_gets_an_explicit_gap_marker_not_silence() {
        let bus = Arc::new(RunLogBroadcaster::new(2));
        let mut sub = bus
            .subscribe(RUN)
            .expect("under the per-run subscriber cap");
        for line in ["one", "two", "three", "four"] {
            bus.publish(RUN, line.to_owned());
        }

        let first = sub.recv().await.expect("the stream is still open");
        assert_eq!(
            first,
            gap_marker(2),
            "two lines were evicted and the subscriber must be told, not skipped"
        );
        // And it resumes from the oldest line still retained, so the marker
        // marks a real hole rather than replacing the whole stream.
        assert_eq!(sub.recv().await.as_deref(), Some("three"));
        assert_eq!(sub.recv().await.as_deref(), Some("four"));
    }

    #[tokio::test]
    async fn a_terminated_runs_channel_is_reaped() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let mut sub = bus
            .subscribe(RUN)
            .expect("under the per-run subscriber cap");
        bus.publish(RUN, "before".to_owned());
        assert_eq!(bus.active_channels(), 1);

        assert!(bus.reap(RUN));
        assert_eq!(bus.active_channels(), 0);
        assert!(
            !bus.reap(RUN),
            "reaping twice is a no-op, not a second drop"
        );

        // The buffered line is still delivered, and only then does the stream
        // end — a reap must not truncate output the run already produced.
        assert_eq!(sub.recv().await.as_deref(), Some("before"));
        assert_eq!(sub.recv().await, None);
    }

    /// **The bound the security review's probe defeated.** 1,000 subscriptions to
    /// attacker-chosen run ids, every one dropped: before `LogSubscription`'s
    /// `Drop` hook this left 1,000 permanent entries, because nothing published
    /// to them (so the publish-side prune never fired) and they were not runs (so
    /// nothing reaped them).
    ///
    /// Deliberately never publishes and never reaps, so the drop hook is the only
    /// thing that can make this pass.
    #[tokio::test]
    async fn dropping_every_subscription_leaves_no_channel_behind() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        for n in 0..1000u128 {
            drop(bus.subscribe(Uuid::from_u128(0xF000_0000 + n)));
        }
        assert_eq!(
            bus.active_channels(),
            0,
            "a dropped subscription must take its channel with it, or the map grows \
             one entry per id anyone ever asked about"
        );
    }

    /// The condition on that prune: a second watcher keeps the channel alive, so
    /// the first to disconnect must not silence it.
    #[tokio::test]
    async fn one_subscriber_leaving_does_not_silence_another() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let mut staying = bus
            .subscribe(RUN)
            .expect("under the per-run subscriber cap");
        drop(bus.subscribe(RUN));
        assert_eq!(bus.active_channels(), 1);

        bus.publish(RUN, "still here".to_owned());
        assert_eq!(staying.recv().await.as_deref(), Some("still here"));
    }

    /// A `Weak`, so a subscription that outlives its broadcaster does not keep
    /// the map alive and does not panic when it goes.
    #[tokio::test]
    async fn a_subscription_may_outlive_its_broadcaster() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let mut sub = bus
            .subscribe(RUN)
            .expect("under the per-run subscriber cap");
        bus.publish(RUN, "last".to_owned());
        drop(bus);

        assert_eq!(sub.recv().await.as_deref(), Some("last"));
        assert_eq!(sub.recv().await, None);
        // And dropping it now is a no-op rather than a dangling upgrade.
        drop(sub);
    }

    /// The cap's **value**, which is the contract: it is the sole bound on how
    /// many SSE connections one caller can hold against one run, each for up to
    /// `MAX_STREAM_DURATION`. Every other test here uses the constant
    /// symbolically, so widening it to any number was green.
    #[test]
    fn the_subscriber_cap_is_the_number_it_promises() {
        assert_eq!(MAX_SUBSCRIBERS_PER_RUN, 16);
    }

    /// **The connection cap**, and the exposure it does and does not close.
    ///
    /// Nothing in the inbound HTTP stack limits concurrency or times a request
    /// out, so before this cap a caller could authenticate, launch one run, and
    /// hold as many SSE connections against it as file descriptors allowed -
    /// each for `MAX_STREAM_DURATION`, which is eight hours.
    ///
    /// The refusal must also not *mint* an entry, or a rejected subscribe would
    /// leave exactly the map growth the cap exists to prevent. That holds
    /// because a refusal implies a pre-existing entry - see `subscribe` - so
    /// what this asserts is the outcome, not a cleanup branch. There is no
    /// cleanup branch; the one that used to be here was dead.
    #[tokio::test]
    async fn a_run_refuses_subscribers_past_the_cap_without_minting_an_entry() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let held: Vec<_> = (0..MAX_SUBSCRIBERS_PER_RUN)
            .map(|_| bus.subscribe(RUN).expect("under the cap"))
            .collect();
        assert_eq!(bus.active_channels(), 1);

        assert!(
            bus.subscribe(RUN).is_none(),
            "the seventeenth subscriber to one run must be refused"
        );
        assert_eq!(
            bus.active_channels(),
            1,
            "and the refusal must not have added a channel"
        );

        // A refused subscribe against a run nobody is watching must leave the
        // map exactly as it found it.
        let other = Uuid::from_u128(0xBEEF);
        let _others: Vec<_> = (0..MAX_SUBSCRIBERS_PER_RUN)
            .map(|_| bus.subscribe(other).expect("under the cap"))
            .collect();
        assert_eq!(bus.active_channels(), 2);

        drop(held);
        assert_eq!(
            bus.active_channels(),
            1,
            "releasing every subscriber still prunes the run's entry"
        );
    }

    /// **The hazard the SSE endpoint's ordering exists to close.**
    ///
    /// `subscribe` mints a channel for any id at all - it holds no repository
    /// and no `SecurityContext`, so it cannot tell a real run from an invented
    /// one, nor the caller's own from another tenant's. That is not a defect in
    /// this type; it is why `api::rest::handlers::runs::stream_run_logs` reads
    /// the run under the caller's scope *before* calling this, and why an
    /// endpoint that reversed the two would hand an unauthorized caller one map
    /// entry per connection, keyed by an id they chose.
    ///
    /// Pinned here rather than at the endpoint because it is a property of this
    /// function, and because a reader auditing the ordering needs to be able to
    /// see that the hazard is real rather than take the endpoint's word for it.
    #[tokio::test]
    async fn subscribe_mints_a_channel_for_an_id_it_cannot_authorize() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let invented = Uuid::from_u128(0xDEAD_BEEF);
        let held = bus
            .subscribe(invented)
            .expect("under the per-run subscriber cap");
        assert_eq!(
            bus.active_channels(),
            1,
            "a run that need not exist now has a channel"
        );
        drop(held);
        assert_eq!(
            bus.active_channels(),
            0,
            "and it lives as long as its holder"
        );
    }

    /// The composed outcome, which is what a caller can rely on: once the last
    /// subscription is gone the channel is not in the map, and publishing to it
    /// is a no-op that mints nothing.
    ///
    /// **This asserted the publish-side prune specifically until 2026-08-14**, by
    /// dropping a subscription and expecting `active_channels() == 1` until the
    /// next publish. That premise died with `LogSubscription`'s `Drop` hook — the
    /// entry is already gone — and rather than reach for a `mem::forget` to
    /// resurrect the old path (which would not work either: a forgotten
    /// `Receiver` keeps the count above zero), the test now pins the property
    /// callers actually have.
    ///
    /// **Reunited with its test 2026-08-15.** A stage-3 hunk inserted a new
    /// test's doc *inside* this block, so these paragraphs sat above
    /// `subscribe_mints_a_channel_for_an_id_it_cannot_authorize` - which does
    /// not do what they describe - and this test had no doc at all. A handler
    /// doc cited the mis-documented test as one of two things pinning the SSE
    /// authorization gap, so a reader following that citation landed here.
    #[tokio::test]
    async fn a_channel_with_no_subscribers_left_is_not_in_the_map() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        drop(bus.subscribe(RUN));
        assert_eq!(bus.active_channels(), 0);

        assert_eq!(bus.publish(RUN, "nobody".to_owned()), 0);
        assert_eq!(bus.active_channels(), 0);
    }

    #[tokio::test]
    async fn runs_do_not_share_a_channel() {
        let bus = Arc::new(RunLogBroadcaster::new(8));
        let mut mine = bus
            .subscribe(RUN)
            .expect("under the per-run subscriber cap");
        let mut theirs = bus
            .subscribe(OTHER)
            .expect("under the per-run subscriber cap");

        bus.publish(RUN, "mine".to_owned());
        assert_eq!(mine.recv().await.as_deref(), Some("mine"));

        bus.reap(RUN);
        assert_eq!(mine.recv().await, None);
        assert_eq!(
            bus.active_channels(),
            1,
            "reaping one run must not touch another's channel"
        );
        bus.publish(OTHER, "theirs".to_owned());
        assert_eq!(theirs.recv().await.as_deref(), Some("theirs"));
    }

    /// A zero capacity would panic inside `tokio::sync::broadcast::channel`.
    #[tokio::test]
    async fn a_zero_capacity_is_clamped_rather_than_panicking() {
        let bus = Arc::new(RunLogBroadcaster::new(0));
        let mut sub = bus
            .subscribe(RUN)
            .expect("under the per-run subscriber cap");
        bus.publish(RUN, "only".to_owned());
        assert_eq!(sub.recv().await.as_deref(), Some("only"));
    }
}
