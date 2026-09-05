//! The runner's log-marker grammar, parsed **incrementally** from a line
//! stream.
//!
//! # Why this exists at all
//!
//! The source system has two producers of per-test data. The first is a live
//! HTTP push: the runner POSTs one payload per test to `VHP_PROGRESS_URL`
//! (`manager/src/services/argo.rs:437-441`, handler
//! `manager/src/routes/runs.rs:1130-1187`). **That path does not exist here and
//! is not planned** — `domain::service::ingest`'s own doc records why: "wiring
//! an ingest route was never in scope, and nothing specifies how a runner would
//! authenticate to it". So every per-test observation this adapter produces has
//! to come out of the second producer: log markers.
//!
//! # Why incremental, and why that is not a detail
//!
//! The source system re-parses the *whole* log text on every 30-second poll
//! cycle (`manager/src/services/run_results_poller.rs:166-181`), so its parsers
//! (`argo.rs:2736-2996`) take a `&str` holding everything. This adapter reads a
//! followed pod log line by line, and
//! [`ExecutionEvent::Log`](crate::domain::ports::run_executor::ExecutionEvent::Log)
//! is per line, so a whole-text parser is the wrong shape. The markers are also
//! **stateful** — a `TEST_RESULT` takes its file from the `TEST_FILE` line
//! printed before the enclosing `TEST_START` — which is why a naive chunk-wise
//! parser is wrong rather than merely slower.
//!
//! # The grammar, and what this parser does with each marker
//!
//! | Marker | Handled |
//! |---|---|
//! | `=== TEST_FILE: <path> ===` | remembered; becomes the next test's file |
//! | `=== TEST_START: <title> ===` | flushes the previous test, opens a new one |
//! | `=== TEST_RESULT: <title> PASSED\|FAILED\|SKIPPED ===` | sets the open test's status |
//! | `=== TEST_LAUNCH_ID: <title> <id> ===` | sets the open test's `ReportPortal` launch id |
//! | `===== N passed in 0.12s =====` | pytest's own summary; sets the open test's duration |
//! | `=== TEST_CASE: <base64-json> ===` | emitted immediately as a **case**-level observation |
//! | `=== TEST_DISCOVERED: {json} ===` | **ignored** — see below |
//!
//! # Two deliberate first-cut divergences from the source system
//!
//! 1. **`TEST_DISCOVERED` seeds are dropped.** The source system inserts a
//!    `PENDING` row per discovered test so its UI can list tests before they
//!    run (`argo.rs:2805-2825`). Here a `TestObservation` goes straight through
//!    `IngestService::apply` into the counters, and `PENDING` is not in the
//!    status vocabulary any counter maps, so seeding would move totals for
//!    tests that never ran. Emitting them is additive and can be done once
//!    ingest has a documented answer for the status.
//! 2. **Per-test log slices are not carried.** The source system stores a
//!    `logs TEXT` column per test (`manager/migrations/001_initial.sql:72`,
//!    filled at `argo.rs:2904`). `TestObservation` has no such field and the
//!    port is frozen, so this is a parity loss the adapter cannot fix — it is
//!    what keeps the UI's "Per-test logs are not available in this deployment"
//!    notice true.
//!
//! # The one-test lag, stated rather than discovered
//!
//! A test's observation is emitted when the **next** `TEST_START` arrives, or
//! at [`MarkerParser::finish`]. It cannot be emitted on its `TEST_RESULT` line,
//! because the duration and the launch id are printed *after* it. That is why
//! `watch` must drain the log to end-of-file before emitting
//! [`ExecutionEvent::Finished`](crate::domain::ports::run_executor::ExecutionEvent::Finished):
//! the last test of every run is in the parser, not the channel, until
//! `finish` is called.

use std::sync::LazyLock;

use base64::Engine;
use regex::Regex;
use serde::Deserialize;

use crate::domain::ports::run_executor::TestObservation;
use crate::infra::executor::argo::naming::normalize_test_path;

/// `=== TEST_FILE: <path> ===` (`argo.rs:2800`).
static FILE_RE: LazyLock<Regex> = LazyLock::new(|| build(r"^=== TEST_FILE: (.+?) ===$"));
/// `=== TEST_START: <title> ===` (`argo.rs:2795`).
static START_RE: LazyLock<Regex> = LazyLock::new(|| build(r"^=== TEST_START: (.+?) ===$"));
/// `=== TEST_RESULT: <title> <STATUS> ===` (`argo.rs:2796-2798`).
///
/// The status alternation is spelled out rather than left as `[A-Za-z]+`, and
/// that is load-bearing: with a lazy title and an open status group, a marker
/// reading "`=== TEST_RESULT: AuthN JWT Happy Path PASSED ===`" parses as title
/// "`AuthN`" and status "`JWT`", and the mismatched title then opens a *second*
/// test. The source system's own pattern has the same alternation for the same
/// reason (`argo.rs:2790-2792`), with three statuses; the three case-level ones
/// are added here, and anything outside the six is simply not a recognised
/// marker — as in the source system.
static RESULT_RE: LazyLock<Regex> = LazyLock::new(|| {
    build(r"^=== TEST_RESULT: (.+?) (PASSED|FAILED|SKIPPED|ERROR|XFAIL|XPASS)(?: .*?)? ===$")
});
/// `=== TEST_LAUNCH_ID: <title> <id> ===` (`argo.rs:2799`).
static LAUNCH_RE: LazyLock<Regex> = LazyLock::new(|| build(r"^=== TEST_LAUNCH_ID: .+? (\S+) ===$"));
/// `=== TEST_CASE: <base64> ===` (`argo.rs:2941`).
static CASE_RE: LazyLock<Regex> = LazyLock::new(|| build(r"^=== TEST_CASE: (\S+) ===$"));
/// pytest's own summary line, e.g. `===== 1 passed in 0.12s =====`
/// (`argo.rs:2801`).
static DURATION_RE: LazyLock<Regex> = LazyLock::new(|| build(r"^=+ .* in ([^=\n]+?) =+$"));

/// Compile a pattern that is a literal in this file.
///
/// # Panics
/// Never for the six patterns above: each is a constant, so a failure here is a
/// compile-time error in source form and would fail on the first line of the
/// first log. Written as an `unwrap_or_else` with a never-matching fallback
/// rather than `expect`, because `clippy::expect_used` is denied workspace-wide
/// and a panic inside a log-parsing task would take down a watcher rather than
/// a test run.
fn build(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| {
        // `\z\A` cannot match any input, so a mis-typed pattern degrades to
        // "this marker is never recognised" instead of killing the watcher.
        #[allow(clippy::unwrap_used)]
        Regex::new(r"\z\A").unwrap()
    })
}

/// One test whose result line may have arrived but whose duration and launch id
/// have not.
#[derive(Debug)]
struct Open {
    title: String,
    file: Option<String>,
    status: Option<String>,
    launch_id: Option<String>,
    duration: Option<String>,
}

/// `=== TEST_CASE: ===`'s decoded payload — the source system's `RawCase`
/// (`argo.rs:2921-2930`), field for field.
#[derive(Debug, Deserialize)]
struct RawCase {
    nodeid: Option<String>,
    file: Option<String>,
    name: Option<String>,
    outcome: Option<String>,
    duration: Option<f64>,
    reason: Option<String>,
    ticket: Option<String>,
}

/// The runner's outcome vocabulary → the stored status vocabulary.
///
/// The source system's `status_of` (`argo.rs:2932-2943`) verbatim, including
/// its pass-through-uppercased default: the status set is **open**, and
/// `TestObservation::status` documents that narrowing it to an enum is wrong.
fn status_of(outcome: &str) -> String {
    match outcome {
        "passed" => "PASSED".to_owned(),
        "failed" => "FAILED".to_owned(),
        "skipped" => "SKIPPED".to_owned(),
        "xfailed" => "XFAIL".to_owned(),
        "xpassed" => "XPASS".to_owned(),
        "error" => "ERROR".to_owned(),
        other => other.to_uppercase(),
    }
}

/// A per-node, line-fed marker parser.
///
/// One instance per [`ExecutionNode`](crate::domain::ports::run_executor::ExecutionNode),
/// because the marker state is per pod log and
/// [`TestObservation::node`] has to name the node that produced it.
#[derive(Debug)]
pub struct MarkerParser {
    node: String,
    current_file: Option<String>,
    open: Option<Open>,
}

impl MarkerParser {
    /// A parser attributing everything it finds to one node.
    pub fn new(node: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            current_file: None,
            open: None,
        }
    }

    /// Feed one log line. Returns the observations that line completed —
    /// usually none.
    pub fn line(&mut self, line: &str) -> Vec<TestObservation> {
        let line = line.trim_end_matches(['\r', '\n']);

        if let Some(captured) = FILE_RE.captures(line) {
            let file = normalize_test_path(&captured[1]);
            self.current_file = Some(file).filter(|value| !value.is_empty());
            return Vec::new();
        }

        if let Some(captured) = START_RE.captures(line) {
            // The previous test's duration and launch id have been seen by now,
            // so this is the point at which it is complete.
            let flushed = self.flush();
            self.open = Some(Open {
                title: captured[1].trim().to_owned(),
                file: self.current_file.clone(),
                status: None,
                launch_id: None,
                duration: None,
            });
            return flushed;
        }

        if let Some(captured) = RESULT_RE.captures(line) {
            let title = captured[1].trim().to_owned();
            let status = status_of(&captured[2].to_lowercase());
            match self.open.as_mut() {
                // A result for the open test: the ordinary path.
                Some(open) if open.title == title => open.status = Some(status),
                // A result with no `TEST_START` before it. Not hypothetical:
                // the source system's `upsert_result` exists precisely because
                // the two marker families can each arrive without the other
                // (`argo.rs:2758-2787`).
                _ => {
                    let flushed = self.flush();
                    self.open = Some(Open {
                        title,
                        file: self.current_file.clone(),
                        status: Some(status),
                        launch_id: None,
                        duration: None,
                    });
                    return flushed;
                }
            }
            return Vec::new();
        }

        if let Some(captured) = LAUNCH_RE.captures(line) {
            if let Some(open) = self.open.as_mut() {
                open.launch_id = Some(captured[1].to_owned());
            }
            return Vec::new();
        }

        if let Some(captured) = CASE_RE.captures(line) {
            // Case-level rows are complete on arrival: everything they carry is
            // inside the marker.
            return self.case(&captured[1]).into_iter().collect();
        }

        if let (Some(captured), Some(open)) = (DURATION_RE.captures(line), self.open.as_mut()) {
            let duration = captured[1].trim();
            if !duration.is_empty() {
                open.duration = Some(duration.to_owned());
            }
        }

        Vec::new()
    }

    /// End of log. Returns the last test's observation, which no `TEST_START`
    /// will ever arrive to flush.
    ///
    /// **`watch` must call this before emitting `Finished`.** Skipping it loses
    /// the last test of every run.
    pub fn finish(&mut self) -> Vec<TestObservation> {
        self.current_file = None;
        self.flush()
    }

    /// Emit the open test, if there is one.
    fn flush(&mut self) -> Vec<TestObservation> {
        let Some(open) = self.open.take() else {
            return Vec::new();
        };
        vec![TestObservation {
            node: self.node.clone(),
            test_file: open.file.unwrap_or_default(),
            test_name: open.title,
            // A test whose `TEST_RESULT` never arrived is what the source
            // system calls `RUNNING` (`argo.rs:2856`) — a section with a start
            // and no result. It is a real state: the pod was killed mid-test.
            status: open.status.unwrap_or_else(|| "RUNNING".to_owned()),
            duration: open.duration,
            launch_id: open.launch_id,
            jira_key: None,
            // File-level row, so `nodeid` is empty by the convention
            // `TestObservation::nodeid` documents at length.
            nodeid: None,
            reason: None,
            ticket: None,
        }]
    }

    /// Decode one `TEST_CASE` marker into a case-level observation.
    ///
    /// Unparseable markers are skipped rather than failing the run, which is
    /// the source system's behaviour and the only safe one: the marker is
    /// produced by a pytest plugin in a test repository nobody here controls.
    fn case(&self, token: &str) -> Option<TestObservation> {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(token)
            .ok()?;
        let text = String::from_utf8(decoded).ok()?;
        let raw: RawCase = serde_json::from_str(&text).ok()?;

        let file = raw
            .file
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                raw.nodeid
                    .as_ref()
                    .map(|nodeid| nodeid.split("::").next().unwrap_or(nodeid).to_owned())
                    .filter(|value| !value.trim().is_empty())
            })
            .map(|value| normalize_test_path(&value))?;

        let nodeid = raw.nodeid.unwrap_or_default();
        let name = raw
            .name
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| nodeid.clone());

        Some(TestObservation {
            node: self.node.clone(),
            test_file: file,
            test_name: name,
            status: status_of(raw.outcome.as_deref().unwrap_or_default().trim()),
            duration: raw.duration.map(|seconds| format!("{seconds:.2}s")),
            launch_id: None,
            jira_key: None,
            nodeid: Some(nodeid).filter(|value| !value.is_empty()),
            reason: raw.reason.filter(|value| !value.trim().is_empty()),
            ticket: raw.ticket.filter(|value| !value.trim().is_empty()),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::MarkerParser;
    use crate::domain::ports::run_executor::TestObservation;
    use base64::Engine;

    /// Feed a whole log to the line-fed parser, the way `watch` does.
    fn parse(log: &str) -> Vec<TestObservation> {
        let mut parser = MarkerParser::new("repo-smoke");
        let mut observed = Vec::new();
        for line in log.lines() {
            observed.extend(parser.line(line));
        }
        observed.extend(parser.finish());
        observed
    }

    /// The runner image's ACTUAL output, captured from a real `pytest -p
    /// pytest_markers -v` run over `deploy/compose/fixtures/smoke-repo` and
    /// pasted here verbatim (only pytest's environment-specific header lines --
    /// `platform`, `rootdir`, `plugins` -- were dropped).
    ///
    /// # Why a captured log and not a hand-written one
    ///
    /// Every other test in this module feeds markers this file's author wrote,
    /// which cannot catch a runner that emits something subtly different. This
    /// one did catch exactly that: the plugin's first version wrote its marker
    /// WITHOUT a leading newline, and pytest's terminal reporter leaves its
    /// progress line (`tests/x.py::t PASSED [ 50%]`) unterminated when the
    /// `pytest_runtest_logreport` hook runs -- so every marker arrived as
    /// `[ 50%]=== TEST_CASE: ... ===`, matched this module's ANCHORED pattern
    /// nowhere, and the run reported zero tests while passing. The runner now
    /// prefixes a newline, and this test is what keeps it there.
    ///
    /// It also pins the four outcomes the fixture exists to produce, including
    /// the two the source system's grammar spells differently from pytest's
    /// (`xfailed` -> `XFAIL`).
    #[test]
    fn the_runner_images_real_pytest_output_parses() {
        let log = r"============================= test session starts ==============================
collecting ... collected 4 items

tests/test_login.py::test_login PASSED                                   [ 25%]
=== TEST_CASE: eyJub2RlaWQiOiJ0ZXN0cy90ZXN0X2xvZ2luLnB5Ojp0ZXN0X2xvZ2luIiwiZmlsZSI6InRlc3RzL3Rlc3RfbG9naW4ucHkiLCJuYW1lIjoidGVzdF9sb2dpbiIsIm91dGNvbWUiOiJwYXNzZWQiLCJkdXJhdGlvbiI6MC4wLCJyZWFzb24iOm51bGx9 ===

tests/test_login.py::test_logout PASSED                                  [ 50%]
=== TEST_CASE: eyJub2RlaWQiOiJ0ZXN0cy90ZXN0X2xvZ2luLnB5Ojp0ZXN0X2xvZ291dCIsImZpbGUiOiJ0ZXN0cy90ZXN0X2xvZ2luLnB5IiwibmFtZSI6InRlc3RfbG9nb3V0Iiwib3V0Y29tZSI6InBhc3NlZCIsImR1cmF0aW9uIjowLjAsInJlYXNvbiI6bnVsbH0= ===

tests/test_login.py::test_password_reset SKIPPED (fixture: proves SK...) [ 75%]
=== TEST_CASE: eyJub2RlaWQiOiJ0ZXN0cy90ZXN0X2xvZ2luLnB5Ojp0ZXN0X3Bhc3N3b3JkX3Jlc2V0IiwiZmlsZSI6InRlc3RzL3Rlc3RfbG9naW4ucHkiLCJuYW1lIjoidGVzdF9wYXNzd29yZF9yZXNldCIsIm91dGNvbWUiOiJza2lwcGVkIiwiZHVyYXRpb24iOjAuMCwicmVhc29uIjoiU2tpcHBlZDogZml4dHVyZTogcHJvdmVzIFNLSVBQRUQgcmVhY2hlcyBxYV9ydW5fdGVzdF9yZXN1bHRzIn0= ===

tests/test_login.py::test_known_broken_login XFAIL (fixture: proves ...) [100%]
=== TEST_CASE: eyJub2RlaWQiOiJ0ZXN0cy90ZXN0X2xvZ2luLnB5Ojp0ZXN0X2tub3duX2Jyb2tlbl9sb2dpbiIsImZpbGUiOiJ0ZXN0cy90ZXN0X2xvZ2luLnB5IiwibmFtZSI6InRlc3Rfa25vd25fYnJva2VuX2xvZ2luIiwib3V0Y29tZSI6InhmYWlsZWQiLCJkdXJhdGlvbiI6MC4wLCJyZWFzb24iOiJmaXh0dXJlOiBwcm92ZXMgWEZBSUwgcmVhY2hlcyBxYV9ydW5fdGVzdF9yZXN1bHRzIn0= ===


=================== 2 passed, 1 skipped, 1 xfailed in 0.02s ====================";

        let observed = parse(log);
        let rows: Vec<(&str, &str, &str)> = observed
            .iter()
            .map(|row| {
                (
                    row.test_file.as_str(),
                    row.test_name.as_str(),
                    row.status.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                ("tests/test_login.py", "test_login", "PASSED"),
                ("tests/test_login.py", "test_logout", "PASSED"),
                ("tests/test_login.py", "test_password_reset", "SKIPPED"),
                ("tests/test_login.py", "test_known_broken_login", "XFAIL"),
            ],
            "four case-level rows, in order, with pytest's outcome words mapped \
             onto the stored vocabulary"
        );

        for row in &observed {
            assert!(
                row.nodeid.is_some(),
                "every row from a TEST_CASE marker is case-level and carries a \
                 nodeid: {row:?}"
            );
            assert_ne!(
                row.test_name, "test_mock_default",
                "the whole point: these are real test names"
            );
        }
        assert_eq!(
            observed[2].reason.as_deref(),
            Some("Skipped: fixture: proves SKIPPED reaches qa_run_test_results"),
            "the skip reason survives the round trip"
        );
        assert!(
            observed.iter().all(|row| row.launch_id.is_none()),
            "this runner emits no TEST_LAUNCH_ID: there is no ReportPortal in \
             this deployment"
        );
    }

    /// The source system's `parse_test_results_captures_pytest_duration`
    /// (`manager/src/services/argo.rs:3210-3229`), re-run against the
    /// incremental parser. This is the parity oracle, not a new assertion.
    #[test]
    fn a_pytest_duration_summary_lands_on_the_open_test() {
        let observed = parse(
            "\
=== TEST_FILE: tests/authn/test_jwt_auth.py ===
=== TEST_START: AuthN JWT Happy Path ===
============================= test session starts ==============================
::test_valid_jwt PASSED

============================== 3 passed in 4.77s ===============================
=== TEST_RESULT: AuthN JWT Happy Path PASSED ===
=== TEST_LAUNCH_ID: AuthN JWT Happy Path 7202 ===
",
        );
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].test_name, "AuthN JWT Happy Path");
        assert_eq!(observed[0].status, "PASSED");
        assert_eq!(observed[0].test_file, "tests/authn/test_jwt_auth.py");
        assert_eq!(observed[0].duration.as_deref(), Some("4.77s"));
        assert_eq!(observed[0].launch_id.as_deref(), Some("7202"));
        assert_eq!(
            observed[0].node, "repo-smoke",
            "every observation names the node whose log produced it"
        );
    }

    /// The source system's
    /// `parse_test_results_keeps_extended_duration_text` (`argo.rs:3286-3303`).
    /// `TestObservation::duration` is documented as the runner's text verbatim,
    /// including this form.
    #[test]
    fn an_extended_duration_is_kept_verbatim() {
        let observed = parse(
            "\
=== TEST_FILE: tests/authn/test_error_handling.py ===
=== TEST_START: Fail-Closed ===
========================= 8 passed in 85.06s (0:01:25) =========================
=== TEST_RESULT: Fail-Closed PASSED ===
",
        );
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].duration.as_deref(), Some("85.06s (0:01:25)"));
    }

    /// The source system's
    /// `parse_test_results_assigns_correct_file_across_multiple_tests`
    /// (`argo.rs:3231-3284`). The off-by-one this guards is the whole reason
    /// the file marker is remembered rather than searched for: the runner
    /// prints the *next* file's `TEST_FILE` before the previous test's
    /// `TEST_RESULT`.
    #[test]
    fn each_test_gets_its_own_file_with_no_off_by_one() {
        let observed = parse(
            "\
=== TEST_FILE: tests/am/test_smoke.py ===
=== TEST_START: Smoke ===
============================== 1 passed in 3.36s ===============================
=== TEST_RESULT: Smoke PASSED ===

=== TEST_FILE: tests/am/test_tenants.py ===
=== TEST_START: Tenants ===
============================== 1 passed in 3.86s ===============================
=== TEST_RESULT: Tenants PASSED ===
",
        );
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].test_name, "Smoke");
        assert_eq!(observed[0].test_file, "tests/am/test_smoke.py");
        assert_eq!(observed[0].duration.as_deref(), Some("3.36s"));
        assert_eq!(observed[1].test_name, "Tenants");
        assert_eq!(observed[1].test_file, "tests/am/test_tenants.py");
        assert_eq!(observed[1].duration.as_deref(), Some("3.86s"));
    }

    /// The property a whole-text parser cannot have, and the reason this module
    /// exists: **the first test's observation must arrive before the log ends.**
    /// If it did not, `watch` would emit every result in one burst at the end
    /// and `cpt-cf-qa-nfr-log-latency` would be met while the results were not.
    #[test]
    fn a_result_is_emitted_before_the_stream_ends() {
        let mut parser = MarkerParser::new("repo-smoke");
        for line in [
            "=== TEST_FILE: tests/a.py ===",
            "=== TEST_START: A ===",
            "=== TEST_RESULT: A PASSED ===",
        ] {
            assert!(
                parser.line(line).is_empty(),
                "nothing is complete until the next test starts"
            );
        }
        let flushed = parser.line("=== TEST_START: B ===");
        assert_eq!(flushed.len(), 1, "A completes when B starts");
        assert_eq!(flushed[0].test_name, "A");
        assert_eq!(flushed[0].status, "PASSED");
        // And B is still open, so `finish` is what completes it.
        assert_eq!(parser.finish().len(), 1);
    }

    /// The failure mode the one-test lag creates if `watch` forgets to call
    /// `finish`: the last test of every run is silently lost.
    #[test]
    fn the_last_test_only_exists_after_finish() {
        let mut parser = MarkerParser::new("n");
        for line in [
            "=== TEST_FILE: tests/a.py ===",
            "=== TEST_START: Only ===",
            "=== TEST_RESULT: Only PASSED ===",
        ] {
            assert!(parser.line(line).is_empty());
        }
        let finished = parser.finish();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].test_name, "Only");
        assert!(
            parser.finish().is_empty(),
            "finish is idempotent, so a double call cannot duplicate a row"
        );
    }

    /// A pod killed mid-test: a `TEST_START` with no `TEST_RESULT`. The source
    /// system calls that `RUNNING` (`argo.rs:2856`) and so does this.
    #[test]
    fn a_test_cut_off_mid_run_is_reported_as_running_rather_than_dropped() {
        let observed = parse("=== TEST_FILE: tests/a.py ===\n=== TEST_START: Cut ===\n");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].status, "RUNNING");
    }

    /// A `TEST_RESULT` with no `TEST_START` before it. The source system's
    /// `upsert_result` exists because the marker families arrive
    /// independently; here it must not be dropped.
    #[test]
    fn a_result_with_no_start_still_produces_an_observation() {
        let observed = parse("=== TEST_FILE: tests/a.py ===\n=== TEST_RESULT: Lone FAILED ===\n");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].test_name, "Lone");
        assert_eq!(observed[0].status, "FAILED");
        assert_eq!(observed[0].test_file, "tests/a.py");
    }

    fn case_marker(json: &str) -> String {
        format!(
            "=== TEST_CASE: {} ===",
            base64::engine::general_purpose::STANDARD.encode(json)
        )
    }

    /// Case-level rows carry the three fields the live progress payload does
    /// not have — `nodeid`, `reason`, `ticket` — which is the whole reason the
    /// source system parses this second marker family
    /// (`run_executor.rs:497-500`).
    #[test]
    fn a_case_marker_produces_a_case_level_observation() {
        let observed = parse(&case_marker(
            r#"{"nodeid":"tests/a.py::TestX::test_y[tls]","file":"tests/a.py","name":"test_y","outcome":"xfailed","duration":1.5,"reason":"open bug","ticket":"VHP-980"}"#,
        ));
        assert_eq!(observed.len(), 1);
        let case = &observed[0];
        assert_eq!(
            case.status, "XFAIL",
            "the source system's status_of mapping"
        );
        assert_eq!(
            case.nodeid.as_deref(),
            Some("tests/a.py::TestX::test_y[tls]")
        );
        assert_eq!(case.test_file, "tests/a.py");
        assert_eq!(case.test_name, "test_y");
        assert_eq!(case.duration.as_deref(), Some("1.50s"));
        assert_eq!(case.reason.as_deref(), Some("open bug"));
        assert_eq!(case.ticket.as_deref(), Some("VHP-980"));
    }

    /// The file falls back to the `nodeid`'s prefix, as in the source system
    /// (`argo.rs:2963-2972`).
    #[test]
    fn a_case_with_no_file_takes_it_from_the_nodeid() {
        let observed = parse(&case_marker(
            r#"{"nodeid":"tests/b.py::test_z","outcome":"passed"}"#,
        ));
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].test_file, "tests/b.py");
        assert_eq!(observed[0].test_name, "tests/b.py::test_z");
        assert_eq!(observed[0].status, "PASSED");
    }

    /// An unknown outcome is passed through uppercased rather than rejected:
    /// `TestObservation::status` is documented as an **open** set.
    #[test]
    fn an_unknown_outcome_is_passed_through_uppercased() {
        let observed = parse(&case_marker(
            r#"{"nodeid":"tests/b.py::t","outcome":"flaked"}"#,
        ));
        assert_eq!(observed[0].status, "FLAKED");
    }

    /// The markers come from a pytest plugin in a test repository nobody here
    /// controls, so a malformed one must be skipped rather than fail a run.
    #[test]
    fn malformed_markers_are_skipped_rather_than_failing_the_run() {
        assert!(parse("=== TEST_CASE: not-base64!! ===").is_empty());
        assert!(parse(&case_marker("{not json")).is_empty());
        assert!(
            parse(&case_marker(r#"{"outcome":"passed"}"#)).is_empty(),
            "a case with neither file nor nodeid cannot be attributed"
        );
    }

    /// `TEST_DISCOVERED` is deliberately ignored — see the module docs. Pinned
    /// so the decision is visible rather than looking like an oversight.
    #[test]
    fn discovered_markers_seed_nothing() {
        assert!(
            parse(r#"=== TEST_DISCOVERED: {"test_file":"tests/a.py","title":"A"} ==="#).is_empty()
        );
    }

    /// Ordinary log output must not be mistaken for a marker, and must not
    /// disturb the open test.
    #[test]
    fn ordinary_output_is_not_a_marker() {
        let observed = parse(
            "\
=== TEST_START: A ===
collected 3 items
tests/a.py::test_one PASSED                                              [ 33%]
=== TEST_RESULT: A PASSED ===
",
        );
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].duration, None);
        assert_eq!(observed[0].test_file, "", "no TEST_FILE marker was printed");
    }

    /// Log lines arrive with their line ending attached when a stream is split
    /// on bytes rather than on lines; a parser that did not trim would never
    /// match the closing `===`.
    #[test]
    fn a_trailing_newline_or_carriage_return_does_not_defeat_a_marker() {
        let mut parser = MarkerParser::new("n");
        assert!(parser.line("=== TEST_START: A ===\r\n").is_empty());
        assert!(parser.line("=== TEST_RESULT: A PASSED ===\n").is_empty());
        let finished = parser.finish();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].status, "PASSED");
    }
}
