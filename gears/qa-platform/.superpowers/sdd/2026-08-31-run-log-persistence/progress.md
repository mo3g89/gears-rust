# SDD ledger — plan: gears/qa-platform/docs/superpowers/plans/2026-08-31-run-log-persistence.md

Spec: gears/qa-platform/docs/superpowers/specs/2026-08-31-run-log-persistence-design.md (read; binding authority)
Branch: feature/qa-platform-specs
Crate: cf-gears-qa-runs at gears/qa-platform/qa-runs/qa-runs

## Setup rulings

Ruling: execute on `feature/qa-platform-specs` rather than a new worktree — why: it is not
main/master, the entire 38-commit arc of this work lives there, and NEXT-SESSION-PROMPT.md
establishes it as the working branch the user resumes from — cost if wrong: a fragmented
branch the user must reconcile by hand; reversible with a rebase.

## Pre-flight conflict scan

### Cross-task pairs (shared file or interface)

| Pair | Produces -> Consumes | Found |
|---|---|---|
| T1 -> T2 | `run_log::{Entity,Model,ActiveModel,Column}`; Model fields run_id/tenant_id/text/lines/updated_at | Clean. T2's append_log names exactly those four Columns and all five ActiveModel fields. |
| T1 -> T2 | `qa_run_logs` DDL -> T2's SQLite test fixture | Clean. Fixture runs the Migrator, so it picks the table up automatically. |
| T2 -> T3 | `append_log(runner, scope, run_id, tenant_id, text, lines)` | Clean. T3's `write()` passes them in that order. |
| T2 -> T3 | `MockRunsRepository: RunLogsRepository` (T2 step 9) | Clean. T3's tests need it and T2 delivers it. |
| T2 -> T6 | `get_log -> Option<ArchivedLog{text,lines}>` | Clean. T6 uses `log.text.lines()`. |
| T3 -> T4 | `LogArchive::record(tenant_id, run_id, line)`, `flush(run_id)` | Clean. T4's call sites match arity and order. |
| T3 -> T5 | `RunLogArchive::new(db, runs, policy_enforcer)`, `flush_due()` | Clean. T5 constructs with three args in that order. |
| T3 -> T4/T5 | `RunLogArchive<R>` must coerce to `Arc<dyn LogArchive>` | Clean. All three LogArchive methods are non-generic, so the trait is dyn-compatible under async_trait. |
| T4 -> T5 | `IngestDeps.archive: Arc<dyn LogArchive>` | Clean. T5 supplies it. |
| T6 vs T1-T5 | files touched | Clean. T6 owns handlers/runs.rs, service/runs.rs, broadcast.rs; no earlier task touches them. |
| T2/T6 -> bounds | `R: RunsRepository + RunLogsRepository` on RunsService and AppServices | **GAP — see ruling below.** The plan's refinement section states the bound but no task step assigns it. |

### Per-task self-consistency

| Task | Found |
|---|---|
| T1 | Consistent. `column_names` includes the `run_id ... PRIMARY KEY ...` line (it starts with the ident, not with "PRIMARY KEY"), so the expected 5-column vec is reachable. Cascade assertion string matches all three DDL constants verbatim. |
| T2 | Consistent, with a prose dependency: the tests name `fixture()`, `fx.scope_for_tenant()`, `fx.seed_run()` which the implementer must build from runs_sea_repo's existing test module. Flagged in dispatch. Newline convention is coherent: `append_log` takes text as given, `record` (T3) is what adds `\n`. |
| T3 | Consistent. `flush_due`'s report arithmetic matches its test (2 runs, 2 lines). Note: `queued` is sampled before `flush`, so a concurrent `record` can undercount the *report* — counters only, never the stored text. Accepted, it is a debug line. |
| T4 | Consistent. |
| T5 | Consistent; its guard asserts on `init`'s source text, precedented at gear.rs:1119. |
| T6 | One omission: the code block logs `archived = archived_flag` but does not show `let archived_flag = archived.is_some();` before the `match` that moves `archived`. Prose says to compute it. Flagged in dispatch. |
| T7 | Verification only, no code. |

### Plan mandates a rubric might call defects

Ruling: T5's `init_constructs_exactly_one_archive` and T2's `a_run_list_query_never_reaches_the_log_table`
assert on source text and on rendered SQL respectively — why: both are precedented in this
crate (gear.rs:1119 and m20260813_000003_initial.rs:765-881) and both guard hazards that no
behavioural test can reach, since the wrong version type-checks and passes — cost if wrong:
a brittle test that needs updating when the surrounding code is renamed. If a reviewer flags
either as a defect, the code stands and the finding is parked.

Ruling: T6 owns adding `+ RunLogsRepository` to the `R` bound on `RunsService` and
`AppServices` — why: T6 is the first and only task whose code needs `RunsService` to reach the
new trait; T4 needs no bound because it holds `Arc<dyn LogArchive>` — cost if wrong: T6's
implementer hits a compile error the plan did not predict and must widen the bound itself,
which is the same change one step later.

## Progress

Ruling: the ledger lives at gears/qa-platform/.superpowers/sdd/<plan>/progress.md, not at the
repo-root path `sdd-workspace` printed — why: the four prior ledgers of this arc are all there
and committed, NEXT-SESSION-PROMPT.md points at that path, and the user resumes sessions from
it; repo-root `.superpowers/` is not git-ignored here, so a ledger there would be both
untracked-but-stageable and invisible to the next session — cost if wrong: a resumed SDD
controller looks in the script's directory first, which is why POINTER.md is left there.

Task 1: implementer DONE_WITH_CONCERNS -> reported as DONE, commit cb36b27c8. 859 tests pass, clippy clean.

Task 1: plan defect found by the implementer and fixed in the plan (commit on the plan file):
Global Constraints said `-p cf-gears-qa-runs`; the package `name` in qa-runs/Cargo.toml is
`qa-runs` with no prefix. All later dispatches corrected.

Task 1: implementer correctly overrode the brief on two points, both verified:
 - MySQL UUID columns are VARCHAR(36) not CHAR(36); SQLite uses TEXT for UUID and timestamp.
   Verified against m20260813_000003_initial.rs, which the brief named as the authority.
 - The brief repeated a claim that `PRAGMA foreign_keys = ON` is load-bearing because
   toolkit-db does not issue it. That file's own "Corrected 2026-08-13" note (line 730-735,
   845-855) retracts it: sqlx's SQLite driver enables FKs by default, so `ON` is a no-op and
   `OFF` is the only honest break-test. Doc comment rewritten to the corrected reasoning.

Task 1: implementer added doc_citations_tests.rs beyond the brief's file list, exempting
`a_run_list_query_never_reaches_the_log_table` — Task 2's guard, cited by the new migration's
module doc. Necessary (the crate has a citation-integrity guard) and self-expiring: that
exemption list is checked bidirectionally, so it will fail once Task 2 creates the real test.
CARRY INTO TASK 2: Task 2 must remove that exemption when it adds the guard.

Task 1: review -> Spec ❌, quality Changes requested. 1 Critical, 1 Minor.
 - CRITICAL: all three dialect bodies declare the FK inline in the column definition
   (`run_id ... REFERENCES qa_runs(id) ON DELETE CASCADE`). MySQL/InnoDB parses and silently
   IGNORES inline REFERENCES; only a table-level FOREIGN KEY clause registers one. So on a
   MySQL deployment qa_run_logs would have no foreign key at all, and the cascade is the
   ENTIRE retention policy by user decision. Verified independently: the authority file uses
   `CONSTRAINT fk_qa_run_queue_run FOREIGN KEY (run_id) REFERENCES ...` for MySQL and a
   table-level `FOREIGN KEY (run_id) REFERENCES ...` for SQLite. The new migration matched
   neither.
 - CRITICAL (same root): `every_dialect_cascades_from_qa_runs` asserts only that the substring
   appears, so it passes against the broken MySQL body. There is no MySQL execution tier in
   this crate, so nothing else would ever catch it. The guard as written cannot fail for the
   dialect that is actually wrong.
 - MINOR: SQLite's inline FK is functionally equivalent but departs from the authority file's
   table-level style — same class of departure that produced the MySQL bug.

Ruling: the Minor enters fix round 1 with the Critical rather than being deferred — why: it is
the same three lines the Critical fix rewrites and the reviewer identifies it as the same root
cause, so fixing it costs nothing now and leaves the file consistent — cost if wrong: none
material; it is a style alignment inside a diff already being rewritten.

Task 1: fix round 1/5 (3 addressed, 0 open; commits cb36b27c8..d40dbccf6). MySQL now uses a
table-level CONSTRAINT fk_qa_run_logs_run; SQLite a table-level FOREIGN KEY; Postgres stays
inline because Postgres honors it, and the strengthened guard exempts Postgres from the
table-level requirement while still asserting the cascade. Re-review traced the original
defect shape against the new assertions and confirmed it goes red at line 190. The DDL-parsing
helper's widened token skip matches m20260813_000003_initial.rs:791 character-for-character.
Task 1: minor (deferred): exclusion-by-leading-token in column_names is fragile if a future
table names a real column `key`/`index`/`unique`. Pre-existing pattern shared with the initial
migration, not introduced here. No column in qa_run_logs collides.
Task 1: complete (commits 542605735..d40dbccf6, review clean)

Task 2: implementer DONE, commit df1a00087. 863 lib tests + 1 integration pass, clippy clean.
Task 2: three brief defects the implementer corrected, all verified by the reviewer:
 - the brief's `Expr::col(..).concat(..)` is sea-query's Postgres-ONLY PgExpr::concat and hits
   `unimplemented!()` on the MySQL and SQLite query builders. Replaced with
   `Func::cust(Alias::new("CONCAT"))`, which every builder renders identically. Verified against
   all three builders and executed against real SQLite. Bundled SQLite is 3.46.0
   (libsqlite3-sys 0.30.1), above the 3.44.0 threshold for a built-in concat().
 - the brief's import paths were wrong: it is `toolkit_db::secure::DBRunner` and
   `toolkit_security::AccessScope`, matching the existing runs_repo.
 - the brief assumed `db_err` needed pub(crate); it was already `pub` in infra/storage/db.rs:96.
Task 2: the implementer extracted `list_query()` from runs_sea_repo::list so the list-query
guard exercises the real builder rather than a hand-copied lookalike. Behaviour-preserving per
the reviewer's diff comparison. This is better than the brief, which risked a guard that
watched a copy.
Task 2: review -> Spec OK, quality Approved. No Critical, no Important. Two Minors.
Task 2: minor (deferred): the module doc's NULL-handling note compares CONCAT against `||` but
not the divergence BETWEEN dialects — MySQL's CONCAT propagates NULL, Postgres' and SQLite's
concat() treat NULL as ''. No functional risk: text is NOT NULL DEFAULT '' and the argument is
&str. Documentation completeness only.
Task 2: minor (deferred): no behavioural test proves a foreign-scoped `append_log` is REJECTED.
Only get_log's cross-tenant read is tested. The write is enforced structurally
(.secure().scope_with on the update, secure_insert's validate_insert_scope on the insert, both
verified by the reviewer in toolkit-db source), but "must not perform an unscoped write" is a
constraint the task called non-negotiable and nothing exercises it end to end.
FLAG FOR FINAL REVIEW: this is the cheapest gap to close on the branch and the most
security-relevant. Consider requiring it before merge.
Task 2: context worth carrying — qa-runs' Cargo.toml:149 enables only ["sqlite","pg"], so the
MySQL dialect body is declaration-only and never executed. Task 1's MySQL FK Critical was still
correct to fix (the body is what a MySQL deployment would run) but its blast radius today is nil.
Task 2: complete (commits d40dbccf6..df1a00087, review clean)

Task 3: implementer DONE, commit 5570a827b. 869 tests pass (863 baseline + 6 new), clippy clean.
Task 3: brief defect — it specified `RunLogArchive::new(db: SerializedDb, ..)`. SerializedDb and
all its methods are `pub(in crate::domain::service)`, unreachable from infra::logs::archive.
Implementer used `Arc<DbProvider>`, which is what gear.rs:263 already builds. Reviewer verified
the substitution loses nothing: with_retry exists to retry SERIALIZABLE contention aborts for
ingest's two concurrent producer transactions, an unrelated race; append_log's UPDATE half is a
single row-locked in-statement CONCAT, and the only remaining race (two writers both seeing 0
rows updated on a run's first append) is caught by run_id PRIMARY KEY and self-heals through
the restore-and-retry path. Confirmed independently against Task 2's own module doc, which had
already designed for exactly this.
Task 3: review -> Spec OK, quality Changes requested. 2 Important, 2 Minor.
 - IMPORTANT: `Pending` derives Debug while holding raw log text. This crate has a RECORDED
   cross-tenant disclosure of exactly this shape — handlers/runs.rs:238 needed skip(logs) on
   #[tracing::instrument] because a Mutex-backed field's Debug printed log content into a trace.
   RunLogArchive not deriving Debug is incidental (PolicyEnforcer/DBProvider aren't Debug), not
   a control. A later #[tracing::instrument] on write(&self, run_id, taken: &Pending) would
   auto-capture taken via Debug.
 - IMPORTANT: restore()'s order-preserving merge branch is never exercised. In the failed-flush
   test the second record() happens AFTER flush() returned, so `newer` is always None. Swapping
   the concatenation order would pass every current test while silently inverting log order for
   any run whose text arrives while a failing flush is in flight. Another guard that cannot fail.
Task 3: minor (deferred, but folded into fix round 1 as one-line doc edits): no documented
precondition against concurrent flush/flush_due for one run (matters for Task 5's tick design);
and flush_due_writes_nothing_when_no_lines_were_recorded's comment implies a zero-line map entry
is reachable when the invariant makes it unreachable.

Ruling: Minors 3 and 4 join fix round 1 rather than being deferred — why: both are one-line doc
edits inside the same file the two Importants rewrite, and Minor 3 is a precondition Task 5's
dispatcher-tick design needs stated before Task 5 is written, not after — cost if wrong: none
material; no behaviour changes.

Task 3: fix round 1/5 (4 addressed, 0 open; commits 5570a827b..dd9ca72c2). Pending now has a
hand-written Debug emitting tenant_id/text_len/lines only, with the prior incident named in the
doc. The merge branch is exercised by a_flush_that_fails_while_a_new_line_arrives_preserves_
arrival_order, gated by an AppendGate in test_support: the re-reviewer traced the happens-before
chain through tokio Notify's permit semantics and confirmed it is forced, not a won race, and
that the assertion would catch the swapped-operand mutation. No Mutex held across an await.
Task 3: complete (commits df1a00087..dd9ca72c2, review clean)

Ruling: Task 4 must serialize flushes per run, and I am carrying it into the dispatch rather
than letting Task 4 land and be caught in review — why: Task 3's newly documented precondition
("one flush per run at a time") is UNREACHABLE today only because flush_due's loop is
sequential, and Task 4 is the change that breaks that. It adds a flush(run_id) call in
IngestService::finish, which can run concurrently with a dispatcher tick's flush_due() for the
same run. Two takes of disjoint windows then race independent appends, and whichever commits
last lands last — so a run's archived log can be silently out of order, which is the one
property this whole feature exists to provide — cost if wrong: if I have misread the
concurrency and the overlap is impossible, the implementer adds an in-flight guard that is
merely redundant, plus one test. Cheap either way.

Task 4: implementer DONE, commit f7ab619d1. 875 tests pass, clippy clean, integration feature
compiles clean.
Task 4: MY RULING WAS CORRECT — the concurrency race was real and reachable. The implementer
confirmed it independently rather than taking the brief's word, and fixed it with a per-run
in_flight set checked-and-set inside the same lock acquisition take() already holds. The
reviewer verified atomicity by reading the code and confirmed the regression test forces a
deterministic interleave through AppendGate's Notify rendezvous rather than winning a race.
Had this landed unfixed, a run's archived log could have been silently out of order — the one
property this feature exists to deliver.
Task 4: review -> Spec OK, quality Changes requested. 1 Important, 2 Minor.
 - IMPORTANT: NoopLogArchive (domain/service/mod.rs:242) is a production path, not cfg(test)
   gated, that accepts every line and archives nothing. If Task 5 wires gear.rs but leaves it
   in AppServices::new, everything compiles, every test passes, and the feature silently does
   nothing. This crate already knows this hazard: gear.rs:55-98's LogWiring exists because a
   second RunLogBroadcaster::new type-checks and "streams nothing ever", and gear.rs:93 adds a
   source-scanning tripwire where the type system cannot close the gap. NoopLogArchive has only
   a doc comment.
 - MINOR: LogArchive's trait doc (mod.rs:184-196) still tells callers they must serialize
   flush themselves "including a future flush-on-finish added beside the periodic tick". Task 4
   IS that caller and does not serialize — the guarantee moved into RunLogArchive::take. The doc
   is now actively misleading.
 - MINOR: in_flight is cleared by an explicit statement after write().await, not a Drop guard.
   A panic unwinding out of write between take() and clear_in_flight leaks the entry, which
   silently stops that run being archived ever again — the "worse failure than the race" the
   brief warned about, reached by unwind instead of a branch.

Ruling: all three enter fix round 1 — why: the Important's fix is `#[deprecated]` on
NoopLogArchive, which turns this crate's existing `clippy -D warnings` gate into a compile-time
forcing function that Task 5 cannot pass without resolving it, so it is strictly better done
now than tracked; Minor 2 is documentation that is wrong rather than merely incomplete, and
would mislead the next implementor; Minor 3 is the exact failure the brief singled out as worse
than the race it was guarding — cost if wrong: three small edits in files already open.

Task 4: fix round 1/5 (3 addressed, 0 open + 1 new minor deferred; commits f7ab619d1..b7dcfddb3).
NoopLogArchive now carries #[deprecated]; exactly two #[allow(deprecated)] exist crate-wide,
both narrowly placed (the impl header and the single struct-literal field). InFlightGuard is a
Drop guard constructed only in take()'s success branch, acquiring the lock inside drop() so no
lock spans the write().await; restore and the flag-clear remain sequential.
Task 4: minor (deferred): archive.rs:184's in_flight field doc still says "Cleared by
[RunLogArchive::clear_in_flight]", a method this same fix deleted. Factually wrong and a broken
intra-doc link. cargo doc is not in this crate's gate, so nothing catches it.

Ruling: the new Minor does not extend Task 4's fix loop — why: the process rule is that only
new Critical/Important breakage in a fix diff joins the open findings; Minors are deferred and
triaged by the final whole-branch review. It is a doc string with no behavioural effect — cost
if wrong: a dangling rustdoc link survives to the final review, which is explicitly pointed at
this list.
Task 4: complete (commits dd9ca72c2..b7dcfddb3, review clean, 1 parked minor)

Ruling: Task 5's dispatch gets a hardened requirement beyond its brief — it must DELETE
NoopLogArchive and both #[allow(deprecated)] attributes, and add a composition guard that fails
if AppServices::new is ever handed a no-op archive — why: the re-reviewer surfaced that the
#[allow(deprecated)] sits on the exact call-site line, so the precise failure mode the tripwire
was built for (Task 5 wires gear.rs but never touches that line) would STILL compile and pass
clippy silently. The tripwire only fires if Task 5's edit happens to touch that construction.
That is materially weaker than the Important implied, and Task 5 is the last chance to close it
before the feature could ship inert — cost if wrong: Task 5 does slightly more work than its
brief describes, and gains a guard the crate's own LogWiring precedent says it should have.

Task 5: implementer DONE, commit 7e378b448. 877 tests (882 with --features integration), clippy
clean both ways.
Task 5: MY HARDENING RULING PAID OFF. The implementer chose the structural control over the
tripwire: NoopLogArchive deleted with both #[allow(deprecated)], and ServiceDeps.archive made a
mandatory Arc<dyn LogArchive> field, so omitting it is error[E0063] at every one of the 8
construction sites. The reviewer independently grepped for any Default/From impl or struct-update
spread that could bypass the mandatory field and found none, and confirmed the only remaining
LogArchive impls are the real RunLogArchive plus test-only doubles behind
`#[cfg(test)] pub(crate) mod test_support`. A no-op archive can no longer exist in a release
build. That is strictly stronger than the #[deprecated] tripwire, whose weakness the Task 4
re-reviewer had exposed.
Task 5: reviewer confirmed the one-instance property end to end and that flush_due's signature
(-> FlushReport, not Result) makes "a flush failure cannot break the tick loop" a type-level
guarantee rather than a convention. Tick re-entrancy confirmed impossible: single loop,
MissedTickBehavior::Delay, body fully awaited before the next tick resolves — which correctly
narrows what in_flight defends against (the tick racing finish, not itself).
Task 5: review -> Spec OK, quality Approved. No Critical, no Important. Two Minors.
Task 5: minor (deferred): test_support.rs:1670's NullLogArchive derives Default but every call
site uses Arc::new(NullLogArchive); dead ceremony.
Task 5: minor (deferred): gear.rs:625 builds a second PolicyEnforcer rather than reusing
AppServices::new's. Benign today — PolicyEnforcer is Clone over an Arc<dyn AuthZResolverClient>
and ::new always sets capabilities: Vec::new(), so two instances from the same authz Arc are
behaviourally identical. Would silently diverge if a later task adds .with_capabilities() to one
call site only. Worth a one-line comment.
Task 5: noted, not a finding: init_constructs_exactly_one_archive is exactly as strong and
exactly as weak as the crate's existing init_builds_exactly_one_log_broadcaster — both are
brittle source-text scans, vulnerable to an aliased constructor or a wrapping helper. Accepted:
it is the same tradeoff the codebase already made, and the type system cannot express "the same
Arc reaches two consumers".
Task 5: complete (commits b7dcfddb3..7e378b448, review clean)

Task 6: implementer DONE, commit 7c0fb695e. 883 tests, clippy clean both feature sets, Postgres
tier 4 pass.
Task 6: review (run on a more capable model, this being the closing code task) -> Spec FAILED on
the break-test rule, quality Changes requested. 3 Important, 4 Minor.
 - IMPORTANT: an_archived_line_cannot_carry_a_second_sse_frame CANNOT FAIL. It asserts
   raw.matches("event:").count() == 1, which is not a property of sanitisation: sse_event uses
   Event::json_data, and its OWN doc (handlers/runs.rs:351-358) says the frame-forgery risk is
   not reachable there. Deleting sanitize_line entirely, or replacing text.lines() with
   once(text), leaves the test green. It was also green before the production change. So the
   brief's "sanitisation applies to archived lines" has zero break-testable coverage. This is
   the FOURTH guard-that-cannot-fail on this branch.
 - IMPORTANT: the embedded-newline unreachability proof is correct today but rests on three
   chained assumptions and should be replaced by an invariant. (a) IngestService::apply is `pub`
   and its own doc invites a future runner-facing HTTP progress endpoint, whose `line` would be
   an arbitrary JSON string field — the comment is the ENTIRE control. (b) The proof's single
   named producer is the argo adapter, which is behind a NON-DEFAULT cargo feature; the default
   executor is the mock. (c) The cost of being wrong is worse than the report's "display
   difference": fragments 2..n of a split line carry NO `[node] ` prefix, which breaks precisely
   the property ingest_tests.rs:1200-1218 says the prefix exists for, and record's
   `entry.lines += 1` per call makes qa_run_logs.lines silently disagree with
   text.lines().count(). (d) The stated cost of fixing is wrong: fan_out_log already does
   format!("[{node}] {line}"), a full O(n) copy, so folding a newline flatten into it is free.
   Reviewer also found an unexamined case the proof claims to have enumerated: input `x\r\r\n`
   yields `x\r`, which str::lines() strips but sanitize_line flattens to a space — cosmetic
   trailing-whitespace divergence, single event either way.
 - IMPORTANT (NEW RISK, and it touches a user decision): the terminal branch now materialises an
   unbounded log TWICE (one String from the DB, then a second full copy as Vec<String> via
   .lines().map(to_owned).collect()), and it `return`s BEFORE logs.subscribe_with_replay, so
   MAX_SUBSCRIBERS_PER_RUN does not apply to it. Before this task the branch was bounded by
   MAX_RETAINED_BYTES_PER_RUN (512 KiB). After it, one authorised caller issuing N concurrent
   GETs on a 200 MB run holds ~2*200MB*N resident with nothing refusing them. Same class as the
   OOM that a_run_list_query_never_reaches_the_log_table exists to prevent, arriving by a
   different door. The design's §8 recorded the no-cap risk for the WRITE side only; this
   read-side amplification is new and recorded nowhere.

Ruling: fix rounds address Importants 1 and 2 fully, and Important 3 only PARTIALLY — stream the
split instead of collecting a second full copy, and document the amplification — but do NOT add
a read-side byte cap. Why: the user decided "no cap" explicitly, with the risk stated to them,
so imposing a cap on the read path would overturn their decision inside an implementation task,
which is not mine to do. Removing the redundant second copy is a pure improvement that changes
no policy. The amplification is surfaced to the user in the final report so the cap decision is
theirs, informed by a fact the design did not have when they made it — cost if wrong: a run
large enough to matter is served from an unbounded single copy rather than an unbounded double
copy, and the user has the read-side fact on the record to act on.
Task 6: minor (deferred): the "second independent gate" doc (runs.rs:655-663) is absolute but
the None => logs.replay(id) fallback arm reads a process-local map with NO scope at all, so a
handler defect skipping svc.runs.get would serve a foreign run's retained tail. Pre-existing,
not introduced here, but the new doc's phrasing should be qualified.
Task 6: minor (deferred): expected_frame_count() is a free function returning 1 under a 15-line
doc comment; inline it.
Task 6: minor (folded into fix round 1): test_support.rs:1664 claims FakeRuns/MockRunsRepository
"do not implement RunLogsRepository" — MockRunsRepository does, at test_support.rs:683. Exactly
the "a comment asserting the archive does not exist" shape Step 6 was chartered to fix, and this
task edited that file.

Task 6: fix round 1/5 (4 addressed, 0 open; commits 7c0fb695e..6a94ea790). The sanitisation test
now asserts on JSON-decoded payloads (no raw \n or \r in any element, and frame count equals
line.lines().count()) with a bare \r added to the fixture, which is what makes the second
assertion non-vacuous; the re-reviewer traced both prescribed mutations by hand and confirmed
each turns it red. fan_out_log flattens \n and \r inside the one format!, so archive and
broadcaster get the identical string by construction and the invariant replaces the proof.
lines_as_events streams from a single Arc<str>; the re-reviewer traced its byte-offset scan
against str::lines() semantics for trailing newline, no trailing newline, blank middle line,
CRLF, lone \r, empty string and leading \n, and confirmed no trailing empty event is possible.
Confirmed NO cap was smuggled in: truncation_marker and MAX_RETAINED_BYTES_PER_RUN appear only
in doc prose as the shape a future cap would take, and still govern the retained tail only.
Task 6: minor (deferred): `node` is still pushed verbatim into the prefix without the \n/\r
flatten applied to `line`. Safe today only because ExecutionNode::name's one production source
is format!("repo-{repo_id}") over a UUID. Pre-existing documented choice (that name is
deliberately not sanitised to a DNS-1123 label), not introduced here, and the new comment's
"never user-authored" claim is accurate as written. Worth revisiting if that name ever gets a
less constrained producer.
Task 6: complete (commits 7e378b448..6a94ea790, review clean)

ALL SIX CODE TASKS COMPLETE. Task 7 is remote deploy/verification only.

================================================================================
CORRECTION TO THE RECORD — Task 2's security claim was half false
================================================================================

The Task 2 entry above records, quoting its reviewer, that a foreign-scoped append_log "is
enforced structurally (.secure().scope_with on the update, secure_insert's validate_insert_scope
on the insert, both verified by the reviewer in toolkit-db source)".

THE INSERT HALF OF THAT IS FALSE. The whole-branch reviewer disproved it with a throwaway probe
test against the SQLite fixture, output quoted in its report:
  PROBE1 foreign first append is_ok=true
  PROBE2 owner sees row=false
  PROBE3 stranger sees row=true
  PROBE4 owner legitimate append is_err=true (UNIQUE constraint failed: qa_run_logs.run_id)

Mechanism, and it is a three-task composition no per-task review could see: the scoped UPDATE
correctly matches 0 rows for a foreign row, so control falls through to secure_insert.
validate_insert_scope (libs/toolkit-db/src/secure/db_ops.rs:190-197) checks the ActiveModel's OWN
tenant_id against the caller's scope — and the caller supplied that tenant, so the check can
never fail. Task 1 chose run_id as the primary key with nothing tying qa_run_logs.tenant_id to
qa_runs.tenant_id. Composed: a foreign tenant can create a run's log row first; the rightful
tenant then can never read it (scoped get_log filters it out) and every legitimate append fails
on the primary key forever.

Each task's choice was locally correct. The invariant that matters — a log row's tenant must
equal its run's tenant — is enforced nowhere.

REACHABILITY TODAY: NONE. The reviewer traced it end to end. The only production attach path is
dispatch.rs:1766-1810, which reads candidate.tenant_id off the run row and refuses nil; watch.rs:432
mints for_result_ingest(target.tenant); there is exactly one production caller of record
(ingest.rs:806) and one of append_log (archive.rs:286).

Also corrected: the Task 2 ledger entry asked whether the missing foreign-write test should block
merge. The test AS I SPECIFIED IT WOULD FAIL, because the property it asserts is not true.

Ruling: a false security claim on the record is worse than a known gap, because the next reader
builds on it — so this correction is written before merge, and the hardening goes into the final
fix wave while qa_run_logs is still unreleased and the constraint is therefore free. Cost if
wrong: a composite foreign key needs a unique index on qa_runs(id, tenant_id), which touches a
table that IS deployed; the index is redundant with an existing primary key so it cannot fail on
existing data, but if the implementer finds otherwise it reports BLOCKED rather than guessing.

Ruling: my Task 6 read-side ruling was right in substance and imprecise in characterisation, and
the whole-branch reviewer is correct to push back. The user declined a SIZE cap on stored text.
A bound on CONCURRENT READERS of that text is a different question, and MAX_SUBSCRIBERS_PER_RUN
already exists — the terminal branch's early return is precisely what bypasses it. Declining to
apply it still stands, because subscribe_with_replay mints a channel a terminal run must not get.
But this must be surfaced to the user as "no bound on concurrent readers", NOT as "the size cap
you declined". Cost if wrong: the user weighs the wrong question.

================================================================================
FINAL FIX WAVE — complete (commits 72bf20c90, f642c205c, e3e367486)
================================================================================

Report: .superpowers/sdd/2026-08-31-run-log-persistence/final-fix-report.md

All eight required items done, nothing declined, plus the high-value I-2.
890 tests default / 897 with --features integration; clippy -D warnings clean both
ways; the 11 pre-existing cargo fmt diffs are unchanged in count and location.

I-1 is closed and NOT blocked. The composite FK
`(run_id, tenant_id) REFERENCES qa_runs(id, tenant_id) ON DELETE CASCADE` is
table-level in all three dialect bodies (Postgres included — a composite key
cannot be inline anywhere, which let the guard drop its Postgres exemption), over
a new unique index on qa_runs(id, tenant_id). The index against the live table is
safe unconditionally: qa_runs.id is already the primary key, so (id, tenant_id) is
unique for any existing rows by implication and the build has no duplicate to fail
on. The one piece I could not close by argument — Postgres accepting a plain unique
INDEX rather than a named UNIQUE constraint as an FK parent key — is now executed
by the new Postgres-tier isolation test rather than reasoned about.

Knock-on worth carrying: m20260813_000003_initial's
every_declared_index_exists_with_the_declared_columns asserts the exact index set
on qa_runs after the whole Migrator runs, so it now lists uq_qa_runs_id_tenant.
A later migration adding an index to qa_runs has to say so there.

Item 6's OpenAPI half: the description text was corrected and the single matching
@description line in qa-platform-ui/src/api/generated/openapi.d.ts was hand-edited,
because `npm run gen:api` needs the gear running on :8087. Comment-only; no
operation id, parameter, status, content type or schema changed. If Task 7's deploy
regenerates that file, expect the line to come back identical plus possibly
unrelated drift.

Still not on the Postgres tier, recorded rather than done: design §6 item 7
(cascade — needs a way to delete a qa_runs row; RunsRepository has no delete and
toolkit_db::Db exposes no raw SQL to gear code) and item 10 (SQL-shape inspection,
fully covered where the DDL constants live).

Beyond the list, two small additions: gear.rs's the_dispatcher_tick_flushes_the_archive
(D-RLP-4's tick call site had no coverage at all — deleting archive.flush_due()
from the dispatcher loop left everything green), and the deletion of
Harness::cancel_run, whose only caller was the test item 7 renamed.

NEXT: Task 7 — remote deploy and verification, per the design's §7 steps 5-10.

================================================================================
SESSION STATE at compaction — 2026-08-31
================================================================================
DONE and committed (HEAD 81f9025ce on feature/qa-platform-specs, NOTHING PUSHED):
 * Tasks 1-6 of the run-log-persistence plan, each reviewed, all fix loops closed.
 * Whole-branch review + its single fix wave + scoped re-review: all 9 findings closed.
 * Design spec amended for the composite FK (f37bb3c05).
 * Chart portability + compose deletion (81f9025ce).
Verified by me directly: clippy 0 both feature sets; cargo test -p qa-runs 890 pass;
all 5 helm tests pass; hardcode guard break-tested red.

IN FLIGHT: deploy to 10.136.20.200, background task bwm7ccne2, log at
/tmp/claude-1000/.../scratchpad/deploy2.txt. Full Rust rebuild (~20 min) because the
chart edits invalidated the Docker cache. THIS RUN ALSO SHIPS the new resolver
auto-detection, so it is the first test of it.

MY MISTAKE, recorded: the FIRST deploy attempt aborted because I edited
deploy-k8s.sh WHILE IT WAS RUNNING. Bash reads scripts incrementally, so changing
byte offsets corrupted its tail ("line 578: This: No such file or directory",
"IMAGE: unbound variable") — and it still reported exit 0, which was worthless.
The cluster was left untouched (helm revision 7, pods 41h old), so no damage.
RULE: never edit a script that is executing.

STILL TO DO:
 1. When deploy2 finishes: run verify-k8s.sh (expect 28 PASS / 0 FAIL), read from a
    FILE not a pipe. Then the acceptance test: restart the gears deployment and
    confirm a finished run's log still serves. Then `npm run gen:api` (needs the gear
    on :8087) to regenerate openapi.d.ts properly — one JSDoc line was hand-edited.
 2. `sudo rmdir` the three empty root-owned dirs (needs the user's password):
    cd gears/qa-platform/deploy && sudo rmdir compose/.generated/k3s-kubeconfig.yaml \
      compose/.generated/qa-environments-argo.yaml compose/.generated/qa-runs-argo.yaml \
      compose/.generated compose
 3. OPEN QUESTION FROM THE USER: Analytics and Dashboard show zero results though
    runs completed with results. Evidence from their screenshots:
      - Analytics' own banner says every outcome count is 0 because NO EXECUTION ROWS
        MATCHED THE SELECTED VERSION, and tells the reader to check the version filter.
      - The Analytics version filter reads 26.5. The Test Runs list's VERSION column
        reads 26.5.0 for every run. "26.5" != "26.5.0" — prime hypothesis is an exact
        string match between a catalog product version and the platform-observed
        version stamped on runs.
      - SECOND, possibly separate symptom: the Dashboard's Recent Failures (24h) shows
        0 and Run Tests By Status is flat at 0, even though monitoring-cms-e2e-tests-1
        failed 5 minutes earlier with 35 failed tests. The Dashboard is product-scoped;
        Test Runs says explicitly it is NOT scoped to the selected product. So either
        those runs are not associated with the selected product, or qa-insights has
        ingested no result rows at all.
      - NOT YET INVESTIGATED against the live database. The settling query is: what
        distinct version strings exist on qa-insights' ingested result rows, and what
        version string does the catalog offer in that filter.
