//! Per-run pending log buffers and the flush that drains them.
//!
//! # The buffer is drained, not a capped tail, and it is not the broadcaster's
//!
//! Reusing `RunLogBroadcaster`'s `retained` map would avoid holding each line
//! twice, and was rejected: `retained` is a *capped tail* that evicts from the
//! front at `MAX_RETAINED_BYTES_PER_RUN`. A run emitting more than that between
//! two flushes would lose its head **before it was ever written**, silently. A
//! drained buffer cannot lose a line it has not yet flushed — that argument
//! still holds, unchanged, and is why this module still does not reuse
//! `retained`.
//!
//! **What used to follow does not hold any more, and is retracted rather than
//! restated.** This section used to conclude that a pending buffer is
//! therefore safe left unbounded, "consistent with the no-cap decision
//! (design §8) and bounded in practice by the flush period." That was true
//! for a log read once. It stopped being true the moment a flapping API
//! server made a watcher re-attach re-read a pod's *whole* log every 5 s
//! while `append_log` concatenated each re-read onto the row — Finding #50 —
//! because "bounded by the flush period" assumes one flush period's worth of
//! *new* output, not the same output arriving again and again between
//! flushes. With that fixed (the preceding commit; the resume position is
//! recovered from a per-node kubelet emission instant, see
//! [`RunLogsRepository::log_resume_positions`](crate::domain::repos::RunLogsRepository::log_resume_positions)),
//! what is left between two flushes really is one flush period's worth of a
//! run's own output — and that can still be enormous for a genuinely chatty
//! run, which is Finding #22. [`MAX_PENDING_BYTES_PER_RUN`] bounds it.
//!
//! That reopens the very objection this section led with: capping *is*
//! "losing a line before it was ever written." The answer is not that the
//! loss stops happening — it does not — but that it stops being silent.
//! Eviction here is followed by exactly the marker `RunLogBroadcaster`
//! already uses for the identical trade,
//! [`broadcast::truncation_marker`](crate::infra::logs::broadcast::truncation_marker),
//! so a run that overflows this buffer has that fact recorded in its own
//! archived text, not merely absent from it.
//!
//! # Cap eviction and resume no longer interact — Task 2 (WS5)
//!
//! This section used to record a genuinely subtle interaction between
//! `Self::enforce_cap`'s eviction and `infra::executor::argo::watch`'s
//! per-node suppression counter (deleted by Task 2, WS5): evicting a
//! node's true first archived line could disable that node's suppression
//! for the rest of the run, and evicting a later line opened a "hole" the
//! last-line guard would flag as a false alarm. Both hazards are gone with
//! that counter itself. A resumed read now seeds `LogParams::since_time` from
//! `RunLogsRepository::log_resume_positions` — an emission instant kubelet
//! itself stamped on the line, recorded in `qa_run_log_positions`
//! independently of whether this buffer's own `text` still holds that
//! line's bytes at flush time. Eviction here still means the evicted bytes
//! are gone from `qa_run_logs`' archived text (recorded by the visible
//! truncation marker below), but it has nothing left to desynchronise from.
//!
//! # What is actually true about the cap, stated rather than asserted
//!
//! Every fix in the round before this one closed a data-loss path that the
//! round before *that* had called impossible in a doc comment, so this is
//! deliberately not written as an absolute:
//!
//! - **One line longer than the cap is kept whole.** [`Pending::enforce_cap`]
//!   refuses to evict a run's last remaining line, so a single line larger
//!   than [`MAX_PENDING_BYTES_PER_RUN`] leaves the buffer one line over
//!   budget rather than emptying it to nothing — the identical trade
//!   `RunLogBroadcaster`'s own `retain` makes, for the identical reason.
//! - **The marker's own bytes are never counted against the cap.** The cap
//!   bounds what `record` receives; the marker is a small, fixed-shape
//!   addition made once, in [`RunLogArchive::write`], never something a
//!   chatty run can grow. `RunLogBroadcaster`'s own byte bound makes the same
//!   choice for the same reason.
//! - **A flush racing a `record` at the cap can produce a marker whose count
//!   is exact but whose claimed position is approximate.** [`RunLogArchive::restore`]
//!   merges a failed flush's already-taken text back in front of whatever
//!   arrived while that write was in flight. If the arriving side
//!   independently overflowed the cap before the merge, its evicted-line
//!   count is summed into the one marker this module ever writes for that
//!   buffer — but that marker still renders at the very front of the merged
//!   text, describing lines that were, in truth, evicted from further in. The
//!   count of lines lost is exact; which lines they were is not, in this one
//!   compound case. It is narrower than it sounds: it needs a flush already
//!   failing *and* another [`MAX_PENDING_BYTES_PER_RUN`] arriving before that
//!   same write settles.
//!
//! # A newline is added here, once
//!
//! `record` takes a line without its terminator and stores it with one `\n`, so
//! the archived text is a plain log file and the SSE read path can split it
//! back into lines with no ambiguity. `RunLogsRepository::append_log` itself
//! takes text exactly as given and adds nothing — that convention is Task 2's,
//! stated on that trait, and this module is the one caller that owes it a
//! trailing newline per line.
//!
//! # Why `db` is `Arc<DbProvider>` and not `SerializedDb`
//!
//! The task brief's illustrative code held a `domain::service::SerializedDb`.
//! That type — and its `new`/`conn`/`with_retry` methods — is
//! `pub(in crate::domain::service)`, so it cannot be named, constructed or
//! called from this module: `infra::logs` is not a descendant of
//! `domain::service`. `gear.rs`'s own wiring (a later task) confirms the
//! intended shape independently: it threads `db.clone()` into
//! `RunLogArchive::new`, where `db` there is the raw
//! `Arc<DBProvider<DomainError>>` built once in `init`, never a `SerializedDb`.
//!
//! It is also the *correct* shape, not just the reachable one.
//! `infra::storage::run_logs_sea_repo`'s own module doc records why
//! `append_log`'s update-then-insert needs no transaction: "the two statements
//! cannot race for one run: flush takes the pending buffer out of the map
//! before writing, so only one caller ever holds a given run's text at a time."
//! `SerializedDb::with_retry`'s `SERIALIZABLE` escalation exists to close races
//! `service::ingest` has and this port does not, so reaching for it here would
//! pay retry overhead for a guarantee this module already provides for free by
//! construction.
//!
//! # Why `RunLogArchive` does not derive `Debug`
//!
//! Also unlike the brief's illustrative code. Neither `authz_resolver_sdk::
//! PolicyEnforcer` nor `toolkit_db::DBProvider` implements `Debug`, so
//! `#[derive(Debug)]` on a struct holding either does not compile.
//! `domain::service::ingest::IngestService` — which holds the same
//! `PolicyEnforcer` — has the identical omission for the identical reason.
//!
//! # One `flush` at a time, per run — enforced here, not merely documented
//!
//! `LogArchive::flush`'s own doc states the precondition this module has to
//! satisfy: two overlapping `flush` calls for the same `run_id` must not each
//! `take` an independent, disjoint slice and issue an independent write,
//! because whichever write *commits* last wins the column regardless of which
//! slice was chronologically newer. That stopped being merely a documented
//! caller obligation the moment [`crate::domain::service::ingest::IngestService::finish`]
//! started calling `flush` beside the periodic tick's `flush_due` — a
//! `finish` and a tick can now genuinely race for the same run, on two
//! different tasks, with nothing serializing them from outside.
//!
//! So this module serializes its own [`Self::take`]: a run with a flush
//! already between `take` and completion refuses a second `take` outright,
//! rather than handing out a second slice. [`ArchiveState`] carries the
//! `in_flight` set for exactly this, guarded by the *same* lock as `pending`
//! so the check-and-set is atomic with the removal — a second lock would
//! reopen the gap between "is anyone else flushing this run" and "now I am".
//! `a_second_concurrent_flush_for_the_same_run_is_a_no_op` drains a real race,
//! gated the same way Task 3's own `a_flush_that_fails_while_a_new_line_
//! arrives_preserves_arrival_order` is, and is red without the guard: removing
//! the `in_flight` check turns it into two real writes whose commit order
//! reverses the stored text.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use time::OffsetDateTime;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{LogResume, RunLogsRepository};
use crate::domain::service::{DbProvider, FlushReport, LogArchive, actions, resources};
use crate::domain::system_actor;
use crate::infra::logs::broadcast::truncation_marker;

/// The most one run's un-flushed buffer may hold, in bytes.
///
/// **The same budget as
/// [`broadcast::MAX_RETAINED_BYTES_PER_RUN`](crate::infra::logs::broadcast::MAX_RETAINED_BYTES_PER_RUN),
/// and a separate constant rather than a reuse of it** — because the two
/// bound different buffers. That one bounds what a late SSE reader can still
/// be shown; this one bounds what has not yet reached the database. They are
/// equal because there is no reason for a run to be allowed more of one than
/// the other, and they are named apart so that changing one does not
/// silently change the other — a future change to either budget is a
/// decision about one buffer, not both, and a single shared constant would
/// make it both without anyone choosing that.
///
/// This bounds `Pending::text` alone. The marker [`RunLogArchive::write`]
/// materializes at flush time is never counted against it — see this
/// module's header, "The marker's own bytes are never counted against the
/// cap."
const MAX_PENDING_BYTES_PER_RUN: usize = 512 * 1024;

/// One run's un-flushed text.
///
/// `Debug` is hand-written below, deliberately, rather than derived — see that
/// impl's own doc.
struct Pending {
    tenant_id: Uuid,
    text: String,
    lines: i64,
    /// Real lines evicted from `text`'s front by [`Self::enforce_cap`], across
    /// this `Pending`'s whole lifetime — reset only by a fresh `take` (see
    /// `RunLogArchive::take`), because a taken buffer starts a new window.
    /// Zero until the cap is first hit, and never decremented: a `restore`
    /// merge sums two `Pending`s' counts rather than losing one — see this
    /// module's header on what that sum's position claim does and does not
    /// mean.
    ///
    /// Deliberately **not** materialized into `text` as it changes.
    /// `RunLogArchive::write` builds the one marker line this produces,
    /// once, from whatever this holds at flush time — matching
    /// `RunLogBroadcaster::Inner::replay`, which builds its own marker fresh
    /// from `RetainedLog::dropped` rather than storing one in `lines`. Doing
    /// it here too, rather than rewriting an embedded marker line on every
    /// eviction, is what keeps a `restore` merge a plain sum instead of a
    /// search for a marker that may or may not already be present in two
    /// different pieces of text.
    dropped: u64,
    /// Each node's most recent emission instant seen since this `Pending`
    /// was last taken — Task 2 (WS5). Independent of [`Self::enforce_cap`]'s
    /// eviction: a node's position only ever moves forward, regardless of
    /// whether older text for it was evicted from the front of `text`.
    /// Absent for a node this buffer has only ever seen with `emitted_at:
    /// None` (a non-Argo executor, or an archived-run line from before this
    /// task).
    positions: HashMap<String, OffsetDateTime>,
}

impl Pending {
    /// Enforce [`MAX_PENDING_BYTES_PER_RUN`] by evicting whole lines from the
    /// front, never the tail — the head of a long log is the part a reader is
    /// least likely to still need, and it is also the end `RunLogBroadcaster`
    /// already evicts from, for the same reason.
    ///
    /// Never evicts a run down to nothing: a single line already longer than
    /// the cap is kept whole rather than emptied out, matching
    /// `RunLogBroadcaster::Inner::retain`'s identical guard
    /// (`log.lines.len() > 1`) and for the identical reason — a stored row
    /// with nothing in it but a marker is a worse failure than one that is
    /// one line over budget.
    fn enforce_cap(&mut self) {
        while self.text.len() > MAX_PENDING_BYTES_PER_RUN && self.lines > 1 {
            // Every stored line was appended by `record` with a trailing
            // `\n` (this module's header, "A newline is added here, once"),
            // so the first `\n` this finds is always a real line boundary,
            // never a false split inside one line's own text.
            let Some(newline_at) = self.text.find('\n') else {
                // Cannot actually happen given the invariant above, and a
                // `break` rather than `unreachable!()` for the same reason
                // `RunLogBroadcaster::evict_retained_runs` gives: the cost of
                // being wrong here is a buffer a little too large, not a
                // panic on the path every archived line takes.
                break;
            };
            self.text.drain(0..=newline_at);
            self.lines -= 1;
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// How many lines [`RunLogArchive::write`] actually archives for this
    /// buffer: `lines` plus one more exactly when a marker is written, since
    /// `lines` itself never counts it (this type's own `dropped` doc). The
    /// one place both `write` (the value it sends to `append_log`) and
    /// `flush`'s success log read this, rather than each recomputing it —
    /// so an operator's log line always matches what the stored row holds.
    fn archived_lines(&self) -> i64 {
        self.lines.saturating_add(i64::from(self.dropped > 0))
    }
}

/// Reports `tenant_id` and `lines`, and `text`'s **length**, never `text`
/// itself.
///
/// This crate has a recorded incident of the same mechanism:
/// `api::rest::handlers::runs::stream_run_logs` needed `skip(svc, ctx, logs)`
/// added to its `#[tracing::instrument]` because `RunLogBroadcaster`'s derived
/// `Debug` walks its `Mutex`-guarded channel map, and an unskipped `logs`
/// argument opened a span enumerating every run streaming on the replica —
/// other tenants' included — inherited by every child span of a request that
/// had named only its own run. `#[derive(Debug)]` on a type holding sensitive
/// per-tenant data is what did that, regardless of the field being a channel
/// map there and raw log text here. A `#[derive(Debug)]` on `Pending` would be
/// the same mechanism waiting to fire again — `write(&self, run_id: Uuid,
/// taken: &Pending)` is exactly the kind of signature a later
/// `#[tracing::instrument]` gets added to, which would auto-capture `taken`
/// through `Debug` and print a tenant's raw log text, and a stray
/// `dbg!(&taken)` would do the same on purpose by accident. Hand-writing the
/// impl (instead of just deleting `#[derive(Debug)]`
/// and leaving `Pending` non-`Debug`) keeps a stuck buffer printable for
/// troubleshooting — `tenant_id`, `lines` and `text.len()` are enough to tell
/// "is this run's buffer growing" without ever printing a tenant's log lines —
/// while making the redaction a property of the type instead of a fact someone
/// has to remember not to undo.
impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending")
            .field("tenant_id", &self.tenant_id)
            .field("text_len", &self.text.len())
            .field("lines", &self.lines)
            .field("dropped", &self.dropped)
            .field("nodes_with_a_position", &self.positions.len())
            .finish()
    }
}

/// Clears `run_id`'s in-flight flag when dropped — on a normal return, an
/// early `?`, **or an unwind**.
///
/// # Why a guard rather than the explicit `clear_in_flight` call this
/// replaced
///
/// The explicit version — call `write`, then call `clear_in_flight` as a
/// statement afterwards — has a gap an explicit statement cannot close: if
/// the `write(..).await` future panics while it is being polled, control
/// never reaches the statement after it, `in_flight` keeps `run_id` forever,
/// and every future `flush` for that run becomes a silent no-op — the run is
/// never archived again. That is a worse failure than the lost-update race
/// this whole mechanism exists to prevent, reached by an unwind instead of a
/// branch, so it needs unwind-safety rather than sequencing.
///
/// A `Drop` impl runs during unwinding as well as on a normal scope exit
/// (this workspace does not build with `panic = "abort"` —
/// `Cargo.toml`'s `[profile.release]` sets `panic = "unwind"` explicitly, and
/// dev/test default to it), so binding one guard per successful [`RunLogArchive::take`]
/// and letting it fall out of scope — rather than calling a
/// `clear_in_flight` method as a statement, which is what this replaced and
/// which no longer exists — is what makes the clear unconditional.
struct InFlightGuard<'a, R> {
    archive: &'a RunLogArchive<R>,
    run_id: Uuid,
}

impl<R> Drop for InFlightGuard<'_, R> {
    fn drop(&mut self) {
        let mut state = self
            .archive
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.in_flight.remove(&self.run_id);
    }
}

/// [`RunLogArchive`]'s pending buffers, plus which runs currently have a
/// flush in flight.
///
/// The two live behind one lock, deliberately: [`RunLogArchive::take`] must
/// check `in_flight` and remove from `pending` as one atomic step, or a second
/// caller could observe "nobody is flushing this run" and "the buffer is
/// still here" as two separately-locked facts that stop being true together.
/// See this module's header.
struct ArchiveState {
    pending: HashMap<Uuid, Pending>,
    /// Runs with a `flush` currently between [`RunLogArchive::take`] and the
    /// moment its write settles — successfully, or via
    /// [`RunLogArchive::restore`]. Cleared by [`InFlightGuard`]'s `Drop` on
    /// both of those paths.
    in_flight: HashSet<Uuid>,
}

/// See this module's header for why `db` is `Arc<DbProvider>`, why this type
/// does not derive `Debug`, and why `pending` and `in_flight` share one lock.
pub struct RunLogArchive<R> {
    state: Mutex<ArchiveState>,
    db: Arc<DbProvider>,
    runs: Arc<R>,
    policy_enforcer: PolicyEnforcer,
}

impl<R> RunLogArchive<R>
where
    R: RunLogsRepository + 'static,
{
    pub fn new(db: Arc<DbProvider>, runs: Arc<R>, policy_enforcer: PolicyEnforcer) -> Self {
        Self {
            state: Mutex::new(ArchiveState {
                pending: HashMap::new(),
                in_flight: HashSet::new(),
            }),
            db,
            runs,
            policy_enforcer,
        }
    }

    /// Take one run's pending text out of the map, leaving no empty entry
    /// behind — unless a flush for it is already in flight, in which case
    /// this refuses outright and takes nothing.
    ///
    /// # Why refuse rather than take a second, disjoint slice
    ///
    /// That second slice is exactly what `LogArchive::flush`'s precondition
    /// forbids: two independent slices mean two independent writes, and
    /// whichever one *commits* last wins the column regardless of which was
    /// chronologically newer. Refusing here, before anything is removed from
    /// `pending`, makes a second concurrent `flush` for the same run a no-op
    /// — see [`LogArchive::flush`](crate::domain::service::LogArchive::flush).
    ///
    /// `in_flight` is checked and set inside the same lock acquisition that
    /// removes from `pending`, so there is no window in which a second caller
    /// could see the run as free.
    ///
    /// Returns the guard that clears `in_flight` alongside the taken text,
    /// rather than clearing it itself or leaving the caller to remember to —
    /// see [`InFlightGuard`]'s own doc for why that has to be a `Drop`, not a
    /// call the caller makes explicitly after the write.
    fn take(&self, run_id: Uuid) -> Option<(Pending, InFlightGuard<'_, R>)> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.in_flight.insert(run_id) {
            return None;
        }
        let Some(taken) = state.pending.remove(&run_id) else {
            // Nothing was actually taken, so no guard is handed out either —
            // clear the flag here rather than leaving the run permanently
            // refused.
            state.in_flight.remove(&run_id);
            return None;
        };
        Some((
            taken,
            InFlightGuard {
                archive: self,
                run_id,
            },
        ))
    }

    /// Put drained text back at the **front** of whatever has arrived since,
    /// preserving order. This is what makes a failed flush lossless.
    ///
    /// `dropped` is summed along with `lines` — see [`MAX_PENDING_BYTES_PER_RUN`]'s
    /// own doc and this module's header for what that sum's position claim
    /// does and does not mean when both sides evicted independently.
    ///
    /// Re-runs [`Pending::enforce_cap`] on the merged buffer before putting
    /// it back. Without this, a merge of two buffers each already within the
    /// cap on their own can land at up to twice it — `MAX_PENDING_BYTES_PER_RUN`'s
    /// own doc says it bounds `Pending::text`, and that claim has to stay
    /// true across this path too, not just across `record`. `enforce_cap`
    /// updates `dropped` itself as it evicts, so a trim here is folded into
    /// the same count `write` reads later — nothing extra to reconcile.
    fn restore(&self, run_id: Uuid, mut taken: Pending) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(newer) = state.pending.remove(&run_id) {
            taken.text.push_str(&newer.text);
            taken.lines += newer.lines;
            taken.dropped = taken.dropped.saturating_add(newer.dropped);
            // `newer` arrived chronologically after `taken` was taken out,
            // so wherever it names a node at all its instant is the later
            // one — a plain overwrite, not a max, is correct here.
            taken.positions.extend(newer.positions);
            taken.enforce_cap();
        }
        state.pending.insert(run_id, taken);
    }

    /// The one database write. Separated from `flush` so `flush` owns only the
    /// take/restore decision.
    ///
    /// Builds the single [`truncation_marker`] line from `taken.dropped` here,
    /// once, rather than reading one out of `taken.text` — `Pending::dropped`'s
    /// own doc explains why nothing embeds it earlier. `Cow` avoids allocating
    /// a new string on the (ordinary) path where nothing was ever dropped.
    async fn write(&self, run_id: Uuid, taken: &Pending) -> Result<(), DomainError> {
        // `TenantBound::new` returns `None` for a nil tenant
        // (`domain::system_actor.rs`). A nil tenant here would mean `record`
        // was called from an unbound context, which is a defect, not a case to
        // accommodate — so it becomes an error rather than archiving under the
        // nil tenant. No tenant id or run id in the message: `DomainError::
        // Internal`'s text is not disclosable (`DomainError::disclosable`), but
        // the *original* error is still what callers of `flush` see, so the
        // message is written as if it were.
        let tenant = system_actor::TenantBound::new(taken.tenant_id)
            .ok_or_else(|| DomainError::Internal("archived log has a nil tenant".to_owned()))?;
        let ctx = system_actor::for_log_archive(tenant);
        // The same resolution `IngestService::run_scope` performs
        // (`domain/service/ingest.rs`): one fresh `qa.run` scope per call, via
        // the enforcer, never hoisted or cached. `DISPATCH` because this is a
        // write to a run's own child data, which is the action `finish`'s
        // write scope uses.
        let scope = self
            .policy_enforcer
            .access_scope(&ctx, &resources::RUN, actions::DISPATCH, Some(run_id))
            .await?;

        let conn = self.db.conn()?;
        // `Pending::archived_lines` adds the marker's one line exactly when
        // `taken.dropped` says one is written below, regardless of how many
        // separate eviction rounds it actually summarizes.
        let text: Cow<'_, str> = if taken.dropped > 0 {
            Cow::Owned(format!(
                "{}\n{}",
                truncation_marker(taken.dropped),
                taken.text
            ))
        } else {
            Cow::Borrowed(taken.text.as_str())
        };
        let lines = taken.archived_lines();
        self.runs
            .append_log(&conn, &scope, run_id, taken.tenant_id, &text, lines)
            .await?;

        // Task 2 (WS5): alongside the text this flush just archived, upsert
        // every node's most recent emission instant this buffer saw — the
        // schema-backed replacement for `infra::executor::argo::watch`'s
        // deleted per-node suppression counter. Only after the append above
        // has succeeded: a
        // position advanced past text that was never actually archived
        // (because `append_log` failed) would tell a resumed read to skip
        // past lines this run's archive does not yet have.
        //
        // One node's upsert failing here does not un-write the text `append_log`
        // just committed, and is not folded into this call's own `Result`:
        // failing the whole flush over a position write would put the text
        // back into `pending` (`RunLogArchive::flush`'s `Err` arm) and
        // re-archive it — a duplicate write for a line that is not the one
        // that actually failed. Logged and otherwise accepted: the cost of
        // a stale position is a re-attach re-emitting more of this node's
        // log than strictly necessary, the same "under-suppress, never
        // lose" direction this whole mechanism already tolerates.
        for (node, last_emitted_at) in &taken.positions {
            if let Err(error) = self
                .runs
                .upsert_log_position(&conn, &scope, run_id, taken.tenant_id, node, *last_emitted_at)
                .await
            {
                warn!(
                    run_id = %run_id,
                    node = %node,
                    %error,
                    "failed to record this node's resume position; a later re-attach may \
                     re-read more of its log than necessary, but no line is lost",
                );
            }
        }

        Ok(())
    }

    /// `run_id`'s currently buffered byte count, `0` if nothing is pending.
    ///
    /// Test-only: nothing on the run path needs to read this, and the only
    /// thing it exists to let a test assert is
    /// [`MAX_PENDING_BYTES_PER_RUN`] actually holding.
    #[cfg(test)]
    fn pending_len(&self, run_id: Uuid) -> usize {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state
            .pending
            .get(&run_id)
            .map_or(0, |pending| pending.text.len())
    }
}

#[async_trait]
impl<R> LogArchive for RunLogArchive<R>
where
    R: RunLogsRepository + 'static,
{
    fn record(
        &self,
        tenant_id: Uuid,
        run_id: Uuid,
        node: &str,
        line: &str,
        emitted_at: Option<OffsetDateTime>,
    ) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = state.pending.entry(run_id).or_insert_with(|| Pending {
            tenant_id,
            text: String::new(),
            lines: 0,
            dropped: 0,
            positions: HashMap::new(),
        });
        entry.text.push_str(line);
        entry.text.push('\n');
        entry.lines += 1;
        entry.enforce_cap();
        if let Some(when) = emitted_at {
            entry.positions.insert(node.to_owned(), when);
        }
    }

    async fn flush(&self, run_id: Uuid) -> Result<(), DomainError> {
        let Some((taken, _in_flight)) = self.take(run_id) else {
            // Either nothing is buffered, or a flush for this run is already
            // in flight — see `Self::take`. Both are a no-op here, which is
            // the precondition's required shape: a second concurrent caller
            // must not take a second, disjoint slice.
            return Ok(());
        };
        // `_in_flight` is not read again — it is held for its `Drop`, which
        // clears the flag `Self::take` set, on every exit from this function
        // including an unwind out of the `.await` below. See
        // `InFlightGuard`'s own doc for why that must be a guard rather than
        // a `clear_in_flight` statement placed after the `match`.
        match self.write(run_id, &taken).await {
            Ok(()) => {
                // `taken.archived_lines()`, not `taken.lines` — the latter
                // never counts the marker `write` may have added, and this
                // is the number an operator would otherwise compare against
                // the stored row and find one short.
                debug!(run_id = %run_id, lines = taken.archived_lines(), "archived run log");
                Ok(())
            }
            Err(error) => {
                // The text goes back rather than being dropped: this is the
                // difference between a delayed write and a lost log.
                self.restore(run_id, taken);
                Err(error)
            }
        }
    }

    async fn flush_due(&self) -> FlushReport {
        let ids: Vec<Uuid> = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.pending.keys().copied().collect()
        };

        let mut report = FlushReport::default();
        for run_id in ids {
            // Sampled before `flush` drains the buffer, so a concurrent
            // `record` between this read and the drain can make the count
            // below undercount what actually got written, and a `flush`
            // that turns out to be a no-op because this run's flush is
            // already in flight elsewhere (see `Self::take`) is counted here
            // as if it had written `queued` lines. Accepted, same as the
            // undercount: this number only feeds a `debug!` line, never a
            // decision, and fixing either would mean locking across the
            // flush — see this module's header on why that lock scope is
            // deliberately narrow.
            let queued = {
                let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.pending.get(&run_id).map_or(0, |p| p.lines)
            };
            match self.flush(run_id).await {
                Ok(()) if queued > 0 => {
                    report.runs += 1;
                    report.lines += u64::try_from(queued).unwrap_or(0);
                }
                Ok(()) => {}
                Err(error) => {
                    report.failed += 1;
                    // The run id and the failure class, never a line of log.
                    warn!(
                        run_id = %run_id,
                        %error,
                        "archiving this run's log failed; text kept for the next pass",
                    );
                }
            }
        }
        report
    }

    async fn resume_positions(
        &self,
        tenant: system_actor::TenantBound,
        run_id: Uuid,
    ) -> Result<LogResume, DomainError> {
        // The same resolution `Self::write` performs for the write half — a
        // fresh `qa.run` scope per call, via the enforcer, never hoisted or
        // cached — except `GET`, not `DISPATCH`: this is a read of the run's
        // own archive, not a write to it.
        let ctx = system_actor::for_log_archive(tenant);
        let scope = self
            .policy_enforcer
            .access_scope(&ctx, &resources::RUN, actions::GET, Some(run_id))
            .await?;
        let conn = self.db.conn()?;
        self.runs.log_resume_positions(&conn, &scope, run_id).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use authz_resolver_sdk::PolicyEnforcer;
    use uuid::Uuid;

    use super::{MAX_PENDING_BYTES_PER_RUN, RunLogArchive};
    use crate::domain::service::LogArchive;
    use crate::domain::service::test_support::{
        MockRunsRepository, PermissiveAuthZ, test_db_provider,
    };
    use crate::infra::logs::broadcast::truncation_marker;

    /// Shared setup for this file's tests. `fixture` is `async` (unlike the
    /// task brief's illustrative call sites, which wrote `let fx = fixture();`
    /// with no `.await`): building the in-memory database `RunLogArchive` needs
    /// a real `DbConn` from — `MockRunsRepository::append_log` ignores its
    /// `DBRunner` argument entirely, but `DBRunner` is sealed
    /// (`toolkit_db::secure::runner`) to types backed by a real connection, so
    /// there is no synchronous way to produce one. `infra::storage::
    /// run_logs_sea_repo`'s own tests build their fixture the same way, with
    /// the same `async fn fixture()` shape.
    struct Fixture {
        archive: RunLogArchive<MockRunsRepository>,
        mock: Arc<MockRunsRepository>,
        tenant: Uuid,
        run_id: Uuid,
    }

    impl Fixture {
        fn stored_text(&self) -> String {
            self.mock.archived_text(self.run_id)
        }

        fn stored_text_for(&self, run_id: Uuid) -> String {
            self.mock.archived_text(run_id)
        }

        fn fail_next_append(&self) {
            self.mock.fail_next_append();
        }

        fn append_calls(&self) -> usize {
            self.mock.append_call_count()
        }

        /// Reaches `archive.state` directly rather than through an accessor:
        /// this test module is declared inside `archive.rs` itself, so it is
        /// the same module as `RunLogArchive` and the private field is in
        /// scope — no production-only accessor needed just for a test.
        fn buffered_runs(&self) -> usize {
            self.archive.state.lock().unwrap().pending.len()
        }
    }

    async fn fixture() -> Fixture {
        let tenant = Uuid::from_u128(0xA11);
        let run_id = Uuid::from_u128(1);
        let db = test_db_provider().await;
        let policy_enforcer = PolicyEnforcer::new(Arc::new(PermissiveAuthZ));
        let mock = Arc::new(MockRunsRepository::empty());
        let archive = RunLogArchive::new(db, Arc::clone(&mock), policy_enforcer);
        Fixture {
            archive,
            mock,
            tenant,
            run_id,
        }
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// A drained buffer is not re-appended. Two flushes with one line between
    /// them must leave that line in the row exactly once.
    #[tokio::test]
    async fn flushing_twice_does_not_duplicate_a_line() {
        let fx = fixture().await;
        fx.archive.record(fx.tenant, fx.run_id, "a", "[a] one", None);
        fx.archive.flush(fx.run_id).await.unwrap();
        fx.archive.flush(fx.run_id).await.unwrap();

        assert_eq!(fx.stored_text(), "[a] one\n");
    }

    /// **A failed flush must not lose text.** The buffer is put back, so a
    /// transient database error costs a delayed write and not a log.
    #[tokio::test]
    async fn a_failed_flush_keeps_its_text_for_the_next_one() {
        let fx = fixture().await;
        fx.archive.record(fx.tenant, fx.run_id, "a", "[a] one", None);

        fx.fail_next_append();
        assert!(
            fx.archive.flush(fx.run_id).await.is_err(),
            "the injected failure must surface",
        );

        fx.archive.record(fx.tenant, fx.run_id, "a", "[a] two", None);
        fx.archive.flush(fx.run_id).await.unwrap();

        assert_eq!(
            fx.stored_text(),
            "[a] one\n[a] two\n",
            "the line from the failed flush must survive",
        );
    }

    /// **Round 1, Important 2.** The test above only proves survival across
    /// two *sequential* flushes: its `record(.., "two")` runs strictly after
    /// `flush(..).await` has already returned, so `restore`'s
    /// order-preserving merge branch (`if let Some(newer) = ...`) never
    /// fires — `newer` is always `None` there. This test forces that branch
    /// to run by gating the mock's `append_log` so it blocks *after* being
    /// entered: `record` then lands a second line onto the now-empty pending
    /// map while the first flush's failing write is still in flight, so
    /// `restore` has to merge a `taken` (pre-failure) with a `newer`
    /// (arrived-during-failure) rather than an empty map. Break-tested by
    /// swapping `restore`'s two `push_str` operands (task-3-report.md).
    #[tokio::test]
    async fn a_flush_that_fails_while_a_new_line_arrives_preserves_arrival_order() {
        let fx = fixture().await;
        fx.archive.record(fx.tenant, fx.run_id, "a", "[a] one", None);

        let gate = fx.mock.block_next_append();
        let flush = fx.archive.flush(fx.run_id);
        let arrive_while_the_write_is_in_flight = async {
            // Only proceeds once the gated `append_log` has already taken
            // "[a] one" out of the pending map (`flush` calls `take` before
            // ever awaiting `write`), so this lands into a fresh entry rather
            // than being concatenated onto text that has not yet been removed.
            gate.wait_for_entry().await;
            fx.archive.record(fx.tenant, fx.run_id, "a", "[a] two", None);
            gate.release();
        };
        let (flush_result, ()) = tokio::join!(flush, arrive_while_the_write_is_in_flight);
        assert!(
            flush_result.is_err(),
            "the gated append must fail, which is what drives restore's merge",
        );

        fx.archive.flush(fx.run_id).await.unwrap();
        assert_eq!(
            fx.stored_text(),
            "[a] one\n[a] two\n",
            "text buffered before the failing write must stay ordered ahead of \
             text that arrived while the write was still in flight",
        );
    }

    /// **Task 4's own guard.** `LogArchive::flush`'s precondition says two
    /// overlapping flushes for the same run must not each take a disjoint
    /// slice — before Task 4, nothing could actually produce that overlap,
    /// because every caller of `flush` was `flush_due`'s sequential loop.
    /// Task 4 puts a second caller beside it: `IngestService::finish` now
    /// flushes too, so a `finish` and a tick's `flush_due` can race for one
    /// run. This test forces exactly that race, reusing the same gate the
    /// test above uses to force `restore`'s merge branch: the first `flush`'s
    /// write is gated open, and while it is in flight a **second** `flush`
    /// call is issued for the *same run*, with a genuinely fresh line queued
    /// for it to steal if the guard is absent.
    ///
    /// **Why the second flush being `Ok` does not by itself prove anything.**
    /// A second flush that wrongly takes its own slice and writes it
    /// successfully also returns `Ok(())` — indistinguishable at that call
    /// site from a correct no-op. What actually discriminates the two is what
    /// they leave behind: a wrongly-successful second write reaches the
    /// repository *before* the first (still-gated) write's failure is even
    /// known, so when the first write's text is finally restored and
    /// reflushed, it lands **after** the second run's text — reversing
    /// chronological order in the stored column. This test's real assertions
    /// are therefore the final `stored_text` order and the total
    /// `append_calls` count, not the second flush's return value.
    ///
    /// **Break-tested**: reverting `Self::take` to unconditionally
    /// `pending.remove(&run_id)` (dropping the `in_flight` check) turns this
    /// red — `stored_text` comes back `"[a] two\n[a] one\n"` and
    /// `append_calls` comes back `3`.
    #[tokio::test]
    async fn a_second_concurrent_flush_for_the_same_run_is_a_no_op() {
        let fx = fixture().await;
        fx.archive.record(fx.tenant, fx.run_id, "a", "[a] one", None);

        let gate = fx.mock.block_next_append();
        let first = fx.archive.flush(fx.run_id);
        let race_a_second_flush_in = async {
            // Only proceeds once the first flush has already taken "[a] one"
            // out of the pending map and is blocked inside its write, exactly
            // as the merge test above synchronizes.
            gate.wait_for_entry().await;
            // A genuinely new, disjoint line: without the guard this is what
            // a second `take` would steal.
            fx.archive.record(fx.tenant, fx.run_id, "a", "[a] two", None);
            let second = fx.archive.flush(fx.run_id).await;
            assert!(
                second.is_ok(),
                "a flush already in flight for this run must be a no-op, not an error"
            );
            gate.release();
        };
        let (first_result, ()) = tokio::join!(first, race_a_second_flush_in);
        assert!(
            first_result.is_err(),
            "the gated append must fail, which is what proves the second flush ran \
             while the first was still in flight rather than strictly after it",
        );

        // The guard must have left "[a] two" untouched in the pending map —
        // not written early by a second, disjoint take.
        assert_eq!(fx.buffered_runs(), 1);
        assert_eq!(
            fx.append_calls(),
            1,
            "only the gated, failing write should have reached the repository so far",
        );

        fx.archive.flush(fx.run_id).await.unwrap();
        assert_eq!(
            fx.stored_text(),
            "[a] one\n[a] two\n",
            "chronological order must survive a flush that raced with one already \
             in flight for the same run",
        );
        assert_eq!(fx.append_calls(), 2);
    }

    /// `flush_due` drains every buffered run, which is how a run retired by
    /// `runs::retire` or `dispatch::transition` gets archived without those
    /// services knowing this port exists.
    #[tokio::test]
    async fn flush_due_drains_every_buffered_run() {
        let fx = fixture().await;
        fx.archive.record(fx.tenant, uuid(10), "a", "[a] first run", None);
        fx.archive.record(fx.tenant, uuid(11), "a", "[a] second run", None);

        let report = fx.archive.flush_due().await;

        assert_eq!(report.runs, 2);
        assert_eq!(report.lines, 2);
        assert_eq!(report.failed, 0);
        assert_eq!(fx.stored_text_for(uuid(10)), "[a] first run\n");
        assert_eq!(fx.stored_text_for(uuid(11)), "[a] second run\n");
    }

    /// An idle tick — no run has ever called `record` — touches the database
    /// zero times.
    ///
    /// This covers only the empty-map case, not a map entry sitting at
    /// `lines: 0`: `record` never inserts one (its first call always writes at
    /// least one line) and `take` always removes an entry outright rather than
    /// leaving one behind at zero, so a zero-line `Pending` is not a state
    /// this port can reach. `Minor (review round 1)`: an earlier version of
    /// this comment implied that state was reachable and covered here.
    #[tokio::test]
    async fn flush_due_writes_nothing_when_no_lines_were_recorded() {
        let fx = fixture().await;
        let report = fx.archive.flush_due().await;
        assert_eq!(report.runs, 0);
        assert_eq!(fx.append_calls(), 0);
    }

    /// An empty buffer left behind by a flush is not kept forever: a finished
    /// run must not hold a map entry for the process's lifetime.
    #[tokio::test]
    async fn a_drained_buffer_is_removed_from_the_map() {
        let fx = fixture().await;
        fx.archive.record(fx.tenant, fx.run_id, "a", "[a] one", None);
        fx.archive.flush(fx.run_id).await.unwrap();
        assert_eq!(fx.buffered_runs(), 0);
    }

    /// **Minor 3, review round 1: `in_flight` must clear on an unwind, not
    /// only on `flush`'s two `Result` exits.** `Self::take`'s `InFlightGuard`
    /// exists for exactly this: without it, a panic between a successful
    /// `take` and the write that follows would leave `run_id` marked
    /// in-flight forever, and every later `flush` for that run would see the
    /// no-op branch and silently stop archiving it — permanently, not just
    /// for one pass.
    ///
    /// This drives the guard directly through `take()` rather than through
    /// `flush()`, which has no panicking branch of its own to trigger — the
    /// same shape `watch_tests::a_panicking_observer_still_frees_its_run`
    /// uses for `WatchRegistry`'s `AttachedSlot`, the identical hazard on a
    /// different guard.
    ///
    /// **What this does not claim:** the text taken before the panic
    /// (`"[a] one"`) is lost, not restored — `restore` is only ever reached
    /// from `flush`'s own `Err` arm, and a panic never reaches it either.
    /// That is a narrower, pre-existing gap than the one being pinned here
    /// (an unreachable run, forever, versus one lost line on an
    /// out-of-process-memory-safety-grade event), and closing it is not this
    /// guard's job.
    ///
    /// **Break-tested**: reverting `Self::take`/`Self::flush` to the
    /// pre-guard shape (an explicit `clear_in_flight` statement after the
    /// `match` in `flush`, as this task originally shipped it) turns this
    /// red — the second `flush` below sees `run_id` still `in_flight` and
    /// silently no-ops, leaving `stored_text` empty instead of `"[a] two\n"`.
    #[tokio::test]
    async fn a_panic_while_a_flush_is_in_flight_still_clears_the_flag() {
        let fx = fixture().await;
        fx.archive.record(fx.tenant, fx.run_id, "a", "[a] one", None);

        // Take directly: holds the `InFlightGuard` `take()` hands out, then
        // panics while it is still alive — mirroring a panic inside `write`
        // between `take` returning and the write settling.
        let taken = fx.archive.take(fx.run_id);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _taken = taken;
            panic!("the write task died mid-flush");
        }));
        assert!(unwound.is_err(), "the panic must actually have happened");

        // If the guard had leaked instead of clearing on unwind, this second
        // flush would see `run_id` still marked in-flight and no-op silently
        // — the run would never be archived again.
        fx.archive.record(fx.tenant, fx.run_id, "a", "[a] two", None);
        fx.archive.flush(fx.run_id).await.unwrap();
        assert_eq!(
            fx.stored_text(),
            "[a] two\n",
            "the flag must have cleared on the panic, or this second flush \
             would have been a silent, permanent no-op",
        );
    }

    /// Not in the brief's Step 1 list, added for the "break-test every guard
    /// you add" constraint: `write`'s `TenantBound::new(..).ok_or_else(..)` is
    /// a guard this task added and nothing above exercises it. A nil tenant
    /// here means `record` was called from an unbound context — a defect, not
    /// a case to accommodate — so `flush` must fail rather than silently
    /// archive under the platform-root sentinel, and it must fail *before*
    /// reaching the repository.
    #[tokio::test]
    async fn a_nil_tenant_is_never_archived() {
        let fx = fixture().await;
        fx.archive.record(Uuid::nil(), fx.run_id, "a", "[a] one", None);

        let error = fx
            .archive
            .flush(fx.run_id)
            .await
            .expect_err("a nil tenant must not be archived");
        assert!(
            matches!(error, crate::domain::error::DomainError::Internal(_)),
            "got {error:?}"
        );
        assert_eq!(
            fx.append_calls(),
            0,
            "the repository must never be reached for a nil tenant",
        );
    }

    /// **A single run's pending buffer is bounded.**
    ///
    /// `record` appended without limit between flushes. The module header
    /// used to defend that as design §8's no-cap decision, "bounded in
    /// practice by the flush period" -- true for a log read once, and it was
    /// the second half of #50's growth while a re-attach re-appended the
    /// whole log every 5 s. That is fixed (the preceding commit); this bounds
    /// the case that is left, a genuinely enormous run.
    ///
    /// The front is dropped, not the tail -- see the marker test below for
    /// the "recorded, not silent" half. Review finding #22.
    #[tokio::test]
    #[allow(
        clippy::integer_division,
        reason = "a whole number of 1 KiB lines guaranteed to overflow the byte cap; \
                  a float here would only make the overflow margin fuzzy"
    )]
    async fn the_pending_buffer_is_capped_per_run() {
        let fx = fixture().await;
        let line = "x".repeat(1024);
        for _ in 0..(MAX_PENDING_BYTES_PER_RUN / 1024 + 64) {
            fx.archive.record(fx.tenant, fx.run_id, "a", &line, None);
        }

        let pending = fx.archive.pending_len(fx.run_id);
        assert!(
            pending <= MAX_PENDING_BYTES_PER_RUN,
            "the pending buffer must stay within {MAX_PENDING_BYTES_PER_RUN} bytes, was {pending}"
        );
    }

    /// **The boundary the cap's own doc names**: one line longer than the
    /// whole cap is kept whole rather than evicted to nothing, matching
    /// `RunLogBroadcaster::Inner::retain`'s identical guard. A buffer with
    /// only a marker and no real output would be a worse failure than one
    /// that is one line over budget.
    #[tokio::test]
    async fn a_single_line_longer_than_the_cap_is_kept_whole() {
        let fx = fixture().await;
        let huge = "y".repeat(MAX_PENDING_BYTES_PER_RUN * 2);
        fx.archive.record(fx.tenant, fx.run_id, "a", &huge, None);

        assert_eq!(
            fx.archive.pending_len(fx.run_id),
            huge.len() + 1,
            "the whole line plus its one trailing newline must survive uncut"
        );
    }

    /// **The loss is recorded, not silent** -- the objection this task's
    /// change to the module header answers. A run that overflows the cap is
    /// flushed with a `truncation_marker` naming exactly how many lines were
    /// evicted, and the archived line count grows by one to cover that
    /// marker line -- `Pending::dropped`'s own doc explains why `lines` itself
    /// never counts it.
    #[tokio::test]
    #[allow(
        clippy::integer_division,
        reason = "a whole number of 1 KiB lines guaranteed to overflow the byte cap; \
                  a float here would only make the overflow margin fuzzy"
    )]
    async fn an_overflowing_run_is_flushed_with_a_visible_truncation_marker() {
        let fx = fixture().await;
        let line = "x".repeat(1024);
        let pushes = MAX_PENDING_BYTES_PER_RUN / 1024 + 64;
        for _ in 0..pushes {
            fx.archive.record(fx.tenant, fx.run_id, "a", &line, None);
        }

        let report = fx.archive.flush_due().await;
        assert_eq!(report.failed, 0, "the write itself must still succeed");

        let stored = fx.stored_text();
        let stored_lines: Vec<&str> = stored.lines().collect();
        assert_eq!(
            stored_lines.len() as u64,
            report.lines + 1,
            "the marker line plus every real line the report says was archived"
        );

        let dropped = u64::try_from(pushes).unwrap().saturating_sub(report.lines);
        assert!(dropped > 0, "this push count must have overflowed the cap");
        assert_eq!(
            stored_lines[0],
            truncation_marker(dropped),
            "the marker must name exactly how many lines were evicted, not a \
             stale or approximate count"
        );
    }

    /// **Fix round 1: `restore` must not merely concatenate past the cap.**
    /// Two buffers each individually within `MAX_PENDING_BYTES_PER_RUN` can
    /// sum to roughly twice it once merged by a failed flush's `restore`;
    /// nothing else would trim that until the next `record`, so a run gone
    /// quiet right after the failure would sit oversized indefinitely.
    /// `restore` re-runs `enforce_cap` on the merged buffer to close that gap.
    #[tokio::test]
    #[allow(
        clippy::integer_division,
        reason = "a whole number of 1 KiB lines, not a byte-exact target"
    )]
    async fn a_restore_merge_past_the_cap_is_trimmed_back_down() {
        let fx = fixture().await;
        let line = "x".repeat(1024);
        // ~60% of the cap each, individually under it, so neither batch's own
        // `record` calls trigger eviction on their own -- only the merge does.
        let batch = MAX_PENDING_BYTES_PER_RUN / 1024 * 3 / 5;

        for _ in 0..batch {
            fx.archive.record(fx.tenant, fx.run_id, "a", &line, None);
        }
        assert!(
            fx.archive.pending_len(fx.run_id) <= MAX_PENDING_BYTES_PER_RUN,
            "the first batch alone must not already be over the cap"
        );

        let gate = fx.mock.block_next_append();
        let flush = fx.archive.flush(fx.run_id);
        let arrive_while_the_write_is_in_flight = async {
            // Only proceeds once the first flush has already taken the first
            // batch out of the pending map and is blocked inside its write —
            // same synchronization the merge-order tests above use.
            gate.wait_for_entry().await;
            for _ in 0..batch {
                fx.archive.record(fx.tenant, fx.run_id, "a", &line, None);
            }
            gate.release();
        };
        let (flush_result, ()) = tokio::join!(flush, arrive_while_the_write_is_in_flight);
        assert!(
            flush_result.is_err(),
            "the gated append must fail, which is what drives restore's merge",
        );

        let pending = fx.archive.pending_len(fx.run_id);
        assert!(
            pending <= MAX_PENDING_BYTES_PER_RUN,
            "restore must trim the merged buffer back under the cap, was {pending}"
        );
    }
}
