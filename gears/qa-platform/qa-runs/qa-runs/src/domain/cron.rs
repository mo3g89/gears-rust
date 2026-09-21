//! Cron evaluation: given a schedule, the due time it last fired for, and a
//! `now`, which due time is outstanding.
//!
//! Pure, like `domain::exclusivity`, `domain::queue` and
//! `domain::state_machine`, and for a sharper reason than any of them: exactly-once
//! firing cannot be tested against a wall clock. `now` is a parameter, so every
//! rule here is a value-in / value-out assertion with no clock, no database and
//! no fixture.
//!
//! # This module does not make firing exactly-once
//!
//! Every instance evaluates every schedule on every tick, and they all reach the
//! same answer — that is all [`next_due`]'s purity buys. What deduplicates is the
//! claim: the unique index behind
//! [`SchedulesRepository::claim_tick`](crate::domain::repos::SchedulesRepository::claim_tick),
//! which lets exactly one caller act on a due time. Read as "the evaluator fires
//! once", the guarantee is false; read as "the evaluator is a function", it is
//! the thing the claim needs in order to be correct.
//!
//! # Five fields, UTC, no timezone
//!
//! The accepted grammar is POSIX five-field cron —
//! `minute hour day-of-month month day-of-week`. That is what
//! `qa_runs_sdk::Schedule::cron` documents, and it is the form an Argo
//! `CronWorkflow`'s `spec.schedule` takes, which is where legacy puts an
//! operator's expression verbatim (`manager/src/services/argo.rs`,
//! `create_cron_workflow`).
//!
//! **`cpt-cf-qa-fr-runs-schedules` says nothing about the expression's shape** —
//! it requires the shared creation path and exactly-once firing, and that is
//! all. Five fields is this gear's decision, read off the SDK model and the
//! legacy hand-off rather than quoted from the PRD.
//!
//! Every instant in and out of this module is UTC. A schedule carries no
//! timezone, so `0 2 * * *` is 02:00 UTC and there is nothing that could make it
//! 02:00 anywhere else. Non-UTC input is not rejected — it is evaluated at the
//! instant it names, and the answer comes back with a UTC offset.
//!
//! **A due time is always a whole second**, because both conversions at the
//! `chrono` boundary work in whole seconds. A `now` of `12:00:00.999` reports
//! `12:00:00` due, not itself. That is not cosmetic: `due_at` is the claim's
//! unique key, so two instances evaluating a few microseconds apart would
//! otherwise compute two different due times for the same occurrence, both
//! claims would succeed, and the run would fire twice — the one failure the
//! unique index cannot catch, because it never sees a collision.
//!
//! # At most one tick per evaluation, and never a back-fill
//!
//! [`next_due`] answers with **the most recent occurrence at or before `now`**.
//! Occurrences older than that are skipped, permanently, however many of them
//! there are.
//!
//! The alternative — enqueue every missed occurrence — is the worse failure for
//! this gear specifically. A control plane down for six hours would, on restart,
//! enqueue six hours of an hourly cluster-upgrade suite onto one platform; the
//! queue is strict FIFO (`domain::queue`) and those runs are exclusive, so they
//! would drain one per dispatcher tick with every other tenant's work stuck
//! behind them. Skipping is recoverable — an operator relaunches. Back-filling a
//! destructive exclusive suite is not.
//!
//! This also matches what the legacy system effectively does. Legacy sets no
//! `startingDeadlineSeconds` on its `CronWorkflow`
//! (`manager/src/services/argo.rs`, `create_cron_workflow`), and with that unset
//! Argo skips missed schedules rather than backfilling them. Legacy *does* set
//! `concurrencyPolicy: "Replace"` there, which is a different question — it
//! bounds overlap, not catch-up — and it governs legacy's trigger workflow,
//! whose whole body is one HTTP POST to the manager API. Nothing in this module
//! corresponds to it.
//!
//! **A skipped occurrence leaves no record an operator can query.**
//! [`skipped_since`] computes the skipped times, and computing them in memory is
//! not recording them. **No production code calls it** — [`next_due`] is the
//! consumed half of this module (`domain::service::schedules`'s `outstanding`),
//! and `skipped_since` is exercised only by this file's own tests. Even if the
//! firing tick did call it, `claim_tick` writes a row for the one due time it
//! claims and for nothing else, while `domain::repos::SchedulesRepository`
//! exposes no tick read of any kind, as its own header says. So "why did my
//! 03:00 run not happen?" has no answer in this system. That is a gap, not a
//! design.
//!
//! # The interlock `claim_tick` depends on
//!
//! Because the answer is the *most recent* occurrence and never an outstanding
//! older one, a claim that is won and then orphaned — the process dies before
//! `record_tick_outcome` — self-heals: the next occurrence is a different
//! `due_at`, so it claims cleanly. An "earliest outstanding occurrence"
//! evaluator would wedge that schedule permanently, because no method on
//! `SchedulesRepository` can advance the cursor past a lost claim.
//! `claim_tick`'s own doc states this dependency on `next_due` by name.
//!
//! # Three places the `cron` crate is not the crontab format, and what is done about each
//!
//! The pinned crate is `cron` 0.17, whose grammar is Quartz-shaped rather than
//! crontab-shaped. All three divergences below were measured against 0.17.0,
//! not read off its documentation.
//!
//! 1. **It wants six or seven fields** (`sec min hour dom month dow [year]`) and
//!    rejects a five-field expression outright. [`parse_cron`] prepends a `0`
//!    seconds field. The expression an operator wrote is what is stored, quoted
//!    in errors, and re-parsed on every evaluation; the six-field form exists
//!    only inside this module.
//! 2. **Its day-of-week ordinals are 1-7 with 1 = Sunday.** The crontab format
//!    numbers them 0-6 with 0 = Sunday. Untranslated, `* * * * 1` — Monday in
//!    a crontab — fires on **Sunday** here, and `* * * * 0` fails to parse.
//!    Legacy hands the identical string to an Argo `CronWorkflow`; **what Argo
//!    makes of it was not measured here**, so the reference this module
//!    translates to is crontab(5), not an observation of Argo.
//!
//!    [`parse_cron`] expands each day-of-week item to **the set of days it
//!    denotes** and renumbers every day in that set. Not its endpoints: `0-7` is
//!    every day, and renumbering the two endpoints yields `1-1`, which is Sunday
//!    alone — a silent wrong-days bug that shipped in the first version of this
//!    module and is now pinned by
//!    `tests::a_day_of_week_range_that_contains_seven_keeps_every_day_in_it`.
//!    Names are resolved here too, against the spellings the crate's own table
//!    accepts, so a mixed `1-FRI` translates rather than being half-shifted.
//!
//!    `7` is accepted as a second spelling of Sunday, which crontab(5) allows.
//!    **Whether Argo's parser accepts `7` has not been measured here**, so this
//!    may be a widening rather than parity. Widening is the safe direction:
//!    nothing already stored can stop parsing because of it.
//! 3. **It matches day-of-month AND day-of-week; the crontab format matches
//!    either.** `0 3 1 * 1` means "the 1st, or any Monday" in a crontab and "a
//!    Monday that is also the 1st" to this crate. [`parse_cron`] therefore
//!    compiles **two** schedules when both day fields are restricted — one with
//!    day-of-week wildcarded, one with day-of-month wildcarded — and every query
//!    takes their union.
//!
//!    **Both directions have to be right.** The most recent occurrence at or
//!    before an instant is the *later* of the two forms' answers; the next one
//!    after an instant is the *earlier*. A union that is correct forward and
//!    wrong backward is the same silent-wrong-days shape as the bug in item 2,
//!    so `tests::either_day_field_matching_is_enough` asserts both.
//!
//!    A field counts as unrestricted when it is *written* as a star form (`*`,
//!    `?`, `*/n`) rather than when the set it denotes happens to be complete —
//!    so `0 3 1-31 * 1` fires every day, not only on Mondays. That is the same
//!    place a crontab draws the line, and it is a decision, not an accident.
//!
//! The `@`-descriptors `@hourly`, `@daily`, `@midnight`, `@weekly`, `@monthly`,
//! `@yearly` and `@annually` expand to their five-field equivalents before
//! anything else looks at the expression, so the field-count rule below never
//! sees them. **`@reboot` is refused**: it means "once, when the daemon starts",
//! which has no due time — there is no occurrence to claim, and on a
//! multi-instance control plane every restart of every replica would qualify.
//! `@every <duration>` is not a cron expression and is not supported either.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use time::OffsetDateTime;

use crate::domain::error::DomainError;

/// `minute hour day-of-month month day-of-week`.
const FIELD_COUNT: usize = 5;

/// The longest expression [`parse_cron`] will look at, in characters.
///
/// **Tied to the column, not chosen for parsing.** `qa_schedules.cron` is
/// `VARCHAR(255)` on Postgres and `MySQL`
/// (`infra::storage::migrations::m20260813_000003_initial`; declared under
/// `m20260813_000004_schedules` before that migration was folded into this
/// one by the docs squash), so a longer
/// expression cannot be stored whatever this function thinks of it. Without the
/// check, a 607-byte expression parses, validates, and fails at the `INSERT`
/// with a driver error — a 500 where the caller deserves a 400. **The `SQLite`
/// tier declares the same column `TEXT`, with no cap, so no unit test in this
/// crate can reproduce that**; the guard is what makes the behaviour the same on
/// all three.
///
/// It also bounds two things that are otherwise unbounded, both measured on the
/// unguarded version: a 500 KB expression of repeated `0-59` lists **parsed
/// successfully** in 97 ms, and `next_due` re-parses on every evaluation of
/// every tick; and a rejected 500 KB expression produced a `DomainError` whose
/// `Display` was half a megabyte, which [`DomainError::disclosable`] classifies
/// as safe to return and to log.
const MAX_EXPRESSION_LEN: usize = 255;

/// The ceiling on how many skipped occurrences [`skipped_since`] will return.
///
/// A cap rather than an honest full answer, because the inputs are a stored cron
/// expression and a stored mark: `* * * * *` against a mark a year old is over
/// half a million occurrences, and this function has no consumer whose need
/// justifies allocating them. What it costs is stated at [`skipped_since`].
pub const MAX_SKIPPED_REPORTED: usize = 1000;

/// A five-field cron expression that has been validated and normalised.
///
/// Opaque on purpose: it holds `cron::Schedule`s whose day-of-week ordinals are
/// not the ones an operator wrote, and there may be two of them (see this
/// module's header). Handing either out would invite a caller to read it in
/// crontab terms and be wrong.
#[derive(Clone, Debug)]
pub struct CronSchedule {
    schedule: cron::Schedule,
    /// The day-of-week half of the either-matches rule.
    ///
    /// `Some` only when both day fields are restricted, in which case
    /// [`Self::schedule`] carries the day-of-month half with day-of-week
    /// wildcarded and this one carries the reverse. `None` is the ordinary case,
    /// where at most one day field restricts anything and the crate's `AND` and
    /// a crontab's `OR` cannot disagree.
    alternative: Option<cron::Schedule>,
}

impl CronSchedule {
    /// The one or two compiled forms whose union this schedule denotes.
    fn forms(&self) -> impl Iterator<Item = &cron::Schedule> {
        std::iter::once(&self.schedule).chain(self.alternative.iter())
    }

    /// The most recent occurrence at or before `now`, or `None` when the
    /// schedule has no occurrence that early.
    ///
    /// Inclusive at `now`, which is the boundary a firing ticker lands on
    /// deliberately: a tick that runs exactly on the minute must find that
    /// minute due, not wait a whole period for the next one.
    fn due_at_or_before(&self, now: OffsetDateTime) -> Option<OffsetDateTime> {
        let now = to_chrono(now)?;
        self.occurrence_at_or_before(now).and_then(from_chrono)
    }

    /// The same question in the crate's own types, so the callers below do not
    /// convert twice.
    fn occurrence_at_or_before(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if self.forms().any(|form| form.includes(now)) {
            return Some(now);
        }
        // `ScheduleIterator` is a `DoubleEndedIterator`, so this is one backward
        // step per form rather than a scan: there is no lookback window to
        // choose, and a rare expression (`0 0 29 2 *`) costs the same as
        // `* * * * *`.
        //
        // **`max`, and `min` in `occurrence_after`.** Union semantics reverse
        // with the direction of travel, and swapping them is a silent
        // wrong-days bug rather than a compile error.
        self.forms()
            .filter_map(|form| form.after(&now).next_back())
            .max()
    }

    /// The earliest occurrence strictly after `after`.
    fn occurrence_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.forms()
            .filter_map(|form| form.after(&after).next())
            .min()
    }
}

/// Validate and normalise a five-field crontab expression.
///
/// # Errors
///
/// [`DomainError::InvalidCron`] for anything that is not five fields after
/// descriptor expansion, for an unsupported or unrecognised `@`-descriptor, for
/// a day-of-week token that is neither an ordinal in 0-7 nor a day name, for a
/// range that runs backwards, and for anything the parser then rejects. The
/// `message` is always this module's own text: the `cron` crate's error
/// `Display` renders only the expression it was given, which is both useless as
/// a diagnostic and — since the expression it was given is the *normalised* one
/// — misleading about what the operator typed.
pub fn parse_cron(expression: &str) -> Result<CronSchedule, DomainError> {
    if expression.chars().count() > MAX_EXPRESSION_LEN {
        return Err(invalid(
            &truncated(expression),
            format!("longer than the {MAX_EXPRESSION_LEN} characters a schedule can store"),
        ));
    }
    let source = expand_descriptor(expression)?.unwrap_or(expression);
    let fields: Vec<&str> = source.split_whitespace().collect();
    if fields.len() != FIELD_COUNT {
        return Err(invalid(
            expression,
            format!(
                "expected {FIELD_COUNT} fields \
                 (minute hour day-of-month month day-of-week), found {}",
                fields.len()
            ),
        ));
    }
    // **As written**, before renumbering. [`is_star_form`] asks what the
    // operator typed, and `renumber_days_of_week` turns `*/2` into the list
    // `1,3,5,7` — which does not begin with a star, so asking the renumbered
    // field would silently put every star-prefixed day-of-week on the union
    // path. `a_field_beginning_with_a_star_is_unrestricted_whatever_follows`
    // caught exactly that, an hour after this line was first written.
    let day_of_week_as_written = fields[4];
    let fields = Fields {
        minute: fields[0],
        hour: fields[1],
        day_of_month: fields[2],
        month: fields[3],
        day_of_week: &renumber_days_of_week(day_of_week_as_written)
            .map_err(|why| invalid(expression, why))?,
    };

    // The either-matches rule: when both day fields restrict something, a
    // crontab fires on `dom OR dow` and this crate would fire on `dom AND dow`.
    // Two schedules, one per side, and every query unions them.
    if is_star_form(fields.day_of_month) || is_star_form(day_of_week_as_written) {
        return Ok(CronSchedule {
            schedule: compile(expression, &fields)?,
            alternative: None,
        });
    }
    Ok(CronSchedule {
        schedule: compile(
            expression,
            &Fields {
                day_of_week: "*",
                ..fields
            },
        )?,
        alternative: Some(compile(
            expression,
            &Fields {
                day_of_month: "*",
                ..fields
            },
        )?),
    })
}

/// The due time this schedule is outstanding for, given the due time it last
/// fired for and the current instant.
///
/// `None` means there is nothing to do. `Some(due_at)` is the value a caller
/// claims and launches: it is an occurrence of `expression`, at or before `now`,
/// strictly later than `last_fired`.
///
/// **At most one**, and always the most recent — see this module's header for
/// why an outage is not backfilled and for what that costs.
///
/// A `last_fired` in the future yields `None` rather than an error. A clock
/// stepping backwards and a restored backup both produce one, and neither is a
/// reason to fail a fleet-wide tick; the schedule simply becomes due again once
/// `now` passes the mark.
///
/// # Errors
///
/// [`DomainError::InvalidCron`] if `expression` does not parse; see
/// [`parse_cron`]. A stored expression can only reach this state by being
/// written before it was validated, or by the validation changing under it.
pub fn next_due(
    expression: &str,
    last_fired: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> Result<Option<OffsetDateTime>, DomainError> {
    let schedule = parse_cron(expression)?;
    let Some(due) = schedule.due_at_or_before(now) else {
        return Ok(None);
    };
    // Both sides truncated, so a mark carrying sub-second precision cannot make
    // an occurrence look outstanding after it fired.
    if last_fired.is_some_and(|last| due.unix_timestamp() <= last.unix_timestamp()) {
        return Ok(None);
    }
    Ok(Some(due))
}

/// The occurrences between `since` and `now` that [`next_due`] will not fire,
/// oldest first.
///
/// The most recent occurrence at or before `now` is excluded, because that is
/// the one that fires. `since` itself is excluded, so passing a schedule's
/// `last_fired_tick` reports exactly what the gap cost. An occurrence that
/// satisfies both day fields of an either-matches expression appears **once**.
///
/// # What this is not
///
/// It is not a record. Nothing persists these times, no tick row exists for
/// them, and no production caller asks for them — see the module header, which
/// carries the consequence. ([`next_due`], the module's other half, is called
/// on every schedule tick; this one is not.) It answers the question in memory
/// for whoever asks it.
///
/// # Two ways the answer is incomplete
///
/// * At most [`MAX_SKIPPED_REPORTED`] entries. A longer gap yields its **oldest**
///   that many occurrences, with no marker distinguishing a truncated answer from
///   a complete one. A full-length result means *possibly* truncated and nothing
///   more — it cannot be told apart from a gap of exactly that many, or of a
///   million. No flag is returned because there is no consumer to need one.
/// * An occurrence the `time` crate cannot represent ends the list early. The
///   `cron` crate's years stop at 2100, so nothing reachable through
///   [`parse_cron`] can trigger this.
///
/// # Errors
///
/// [`DomainError::InvalidCron`] if `expression` does not parse; see
/// [`parse_cron`].
pub fn skipped_since(
    expression: &str,
    since: OffsetDateTime,
    now: OffsetDateTime,
) -> Result<Vec<OffsetDateTime>, DomainError> {
    let parsed = parse_cron(expression)?;
    let (Some(since), Some(now)) = (to_chrono(since), to_chrono(now)) else {
        return Ok(Vec::new());
    };
    let Some(fires) = parsed.occurrence_at_or_before(now) else {
        return Ok(Vec::new());
    };

    // A cursor rather than an iterator chain, because the union of two forms has
    // to come out ordered and deduplicated: `occurrence_after` answers with the
    // earlier of the two, and advancing the cursor past it collapses an
    // occurrence both forms match into one entry.
    let mut skipped = Vec::new();
    let mut cursor = since;
    while skipped.len() < MAX_SKIPPED_REPORTED {
        let Some(occurrence) = parsed.occurrence_after(cursor) else {
            break;
        };
        if occurrence >= fires {
            break;
        }
        let Some(converted) = from_chrono(occurrence) else {
            break;
        };
        skipped.push(converted);
        cursor = occurrence;
    }
    Ok(skipped)
}

/// The five fields of an expression, after day-of-week renumbering.
///
/// Named rather than five positional `&str`s because [`compile`] is called
/// three times with two of them deliberately different, and the compiler cannot
/// tell any two apart: transposing `day_of_month` and `day_of_week` in the
/// union's second call would build the intersection of the wrong pair and fire
/// on the wrong days, silently. `..fields` is what makes the
/// one-side-wildcarded pattern read at a glance.
#[derive(Clone, Copy)]
struct Fields<'a> {
    minute: &'a str,
    hour: &'a str,
    day_of_month: &'a str,
    month: &'a str,
    /// Already renumbered into the crate's ordinals by
    /// [`renumber_days_of_week`], or a literal `*` for the union's
    /// day-of-month half.
    day_of_week: &'a str,
}

/// Build one `cron::Schedule` from already-translated fields.
fn compile(expression: &str, fields: &Fields<'_>) -> Result<cron::Schedule, DomainError> {
    let Fields {
        minute,
        hour,
        day_of_month,
        month,
        day_of_week,
    } = *fields;
    let normalised = format!("0 {minute} {hour} {day_of_month} {month} {day_of_week}");
    cron::Schedule::from_str(&normalised).map_err(|_| {
        // The crate's error carries no diagnostic at all (see `parse_cron`), and
        // this is where every field except day-of-week fails: a zero
        // day-of-month, a `*/0` step, a negative number, and the Quartz
        // spellings `L` and `1W` that somebody migrating will try all land here.
        // Naming the fields and their ranges is most of what an operator needs,
        // and it is all this layer honestly knows.
        invalid(
            expression,
            "not a valid five-field cron expression: minute 0-59, hour 0-23, \
             day-of-month 1-31, month 1-12, day-of-week 0-7. Lists (`1,15`), \
             ranges (`1-5`) and steps (`*/15`) are allowed in any field; `L`, \
             `W` and `#` are not"
                .to_owned(),
        )
    })
}

/// The head of an over-long expression, for quoting in an error.
///
/// [`DomainError::InvalidCron`] interpolates its `expression` into `Display`
/// and the variant is disclosable, so the whole of an unbounded input would be
/// returned to the caller and written to the log. Only the length guard needs
/// this: every other refusal happens after it, so it already has a bounded
/// expression to quote.
fn truncated(expression: &str) -> String {
    const HEAD: usize = 64;
    let head: String = expression.chars().take(HEAD).collect();
    format!("{head}...")
}

fn invalid(expression: &str, message: String) -> DomainError {
    DomainError::InvalidCron {
        expression: expression.to_owned(),
        message,
    }
}

/// Expand an `@`-descriptor to its five-field equivalent.
///
/// `Ok(None)` for anything that is not a descriptor, which is then judged as an
/// ordinary expression. Matching is case-insensitive, so `@Daily` is not refused
/// on a technicality it would take an operator a while to spot.
///
/// # Errors
///
/// [`DomainError::InvalidCron`] for `@reboot` and for any other unrecognised
/// `@`-word. Both name the supported set, because "expected 5 fields, found 1"
/// is a useless thing to tell somebody who typed `@wekly`.
fn expand_descriptor(expression: &str) -> Result<Option<&'static str>, DomainError> {
    const SUPPORTED: &str = "the supported descriptors are @hourly, @daily, @midnight, @weekly, \
                             @monthly, @yearly and @annually";

    let trimmed = expression.trim();
    if !trimmed.starts_with('@') {
        return Ok(None);
    }
    Ok(Some(match trimmed.to_ascii_lowercase().as_str() {
        "@hourly" => "0 * * * *",
        "@daily" | "@midnight" => "0 0 * * *",
        "@weekly" => "0 0 * * 0",
        "@monthly" => "0 0 1 * *",
        "@yearly" | "@annually" => "0 0 1 1 *",
        // Not an oversight and not a parser limitation: `@reboot` means "once,
        // when the daemon starts", and this evaluator has no such event to
        // anchor to. There is no due time, so there is nothing to claim — and
        // on a multi-instance control plane every replica restart would be one.
        "@reboot" => {
            return Err(invalid(
                expression,
                format!("@reboot has no due time, so a schedule cannot fire for it; {SUPPORTED}"),
            ));
        }
        _ => {
            return Err(invalid(
                expression,
                format!("unrecognised descriptor; {SUPPORTED}"),
            ));
        }
    }))
}

/// Whether a field is *written* starting with a star, which is what decides
/// whether the either-matches rule applies to the two day fields.
///
/// # Which reference this follows, since the two disagree
///
/// crontab(5)'s prose says the rule applies when both day fields "are
/// restricted (ie, are not `*`)". Vixie's implementation does not test the
/// field against `*`; it sets its star flags from the field's **first
/// character**. The two part company on exactly two shapes, and this function
/// follows the implementation for both:
///
/// * `*/2` — a star form, so unrestricted. The prose reading would call it
///   restricted, because it is not the single character `*`.
/// * `*,1` — also a star form here, so unrestricted, because it begins with a
///   star. The prose reading would call it restricted for the same reason.
///
/// Following one reference for one shape and the other for the other is what
/// this function did until it was measured; the two doc sites both claimed the
/// single rule, and the disagreement was a silent wrong-days answer.
/// `tests::a_field_beginning_with_a_star_is_unrestricted_whatever_follows`
/// pins both shapes in both day fields.
///
/// A field that spans everything without being written as a star — `1-31`,
/// `0-6` — is still a restriction under either reading, so `0 3 1-31 * 1` fires
/// every day rather than only on Mondays.
///
/// **This is the OR/AND question and nothing else.** The star checks in
/// [`renumber_days_of_week`] and [`expand_day_item`] answer a different one —
/// what a day-of-week field's *set of days* is — and must not be folded into
/// this one: `*/2` is unrestricted here and still needs its days enumerated
/// there.
fn is_star_form(field: &str) -> bool {
    field == "?" || field.starts_with('*')
}

/// Rewrite a crontab day-of-week field into the `cron` crate's 1 = Sunday
/// ordinals.
///
/// **Expands each item to the set of days it denotes**, renumbers every day in
/// the set, and emits the union as a sorted list of the crate's ordinals. The
/// obvious alternative — rewriting each range's endpoints in place — is wrong
/// wherever `7` appears, because `7` renumbers *below* the days before it:
/// `0-7` is every day and its endpoints alone give `1-1`, Sunday. That bug
/// shipped once.
///
/// A bare `*` or `?` is returned untouched; there is nothing in it to renumber,
/// and `?` is a spelling only the crate understands. **This check is not
/// [`is_star_form`] and must not become it**: the question here is which days a
/// field denotes, so `*/2` and `*,1` are star-*prefixed* and still have to be
/// enumerated. [`is_star_form`] answers the unrelated OR/AND question and is the
/// only authority on that one.
fn renumber_days_of_week(field: &str) -> Result<String, String> {
    if field == "*" || field == "?" {
        return Ok(field.to_owned());
    }
    let mut ordinals: Vec<u32> = Vec::new();
    for item in field.split(',') {
        for day in expand_day_item(item)? {
            // 0-6 shift up by one and 7 (a crontab's second spelling of Sunday)
            // wraps to the crate's 1, which is what `% 7` does.
            ordinals.push(day % 7 + 1);
        }
    }
    // Tidiness, **not** correctness, and worth saying so: the crate collects a
    // field's ordinals into an `OrdinalSet`, so `"4,2,1,1,2"` and `"1,2,4"`
    // compile to the same schedule. Removing either line here is an equivalent
    // mutation — measured, not assumed — and no test should be manufactured to
    // catch it. Sorted output is for the human reading a debug dump.
    ordinals.sort_unstable();
    ordinals.dedup();
    Ok(ordinals
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(","))
}

/// The crontab day numbers one comma-separated day-of-week item denotes, in
/// 0-7 with both 0 and 7 meaning Sunday.
fn expand_day_item(item: &str) -> Result<Vec<u32>, String> {
    let (base, step) = match item.split_once('/') {
        Some((base, step)) => {
            let step: u32 =
                step.parse().ok().filter(|step| *step > 0).ok_or_else(|| {
                    format!("day-of-week step '{step}' must be a positive number")
                })?;
            (base, Some(step))
        }
        None => (item, None),
    };

    let (low, high) = if base == "*" || base == "?" {
        // "every day", as the base of a possible step. Not [`is_star_form`],
        // which answers the OR/AND question rather than this one; see
        // [`renumber_days_of_week`].
        (0, 6)
    } else if let Some((low, high)) = base.split_once('-') {
        (day_ordinal(low)?, day_ordinal(high)?)
    } else {
        let point = day_ordinal(base)?;
        // `n/k` counts from `n` to the end of the week, the reading a crontab
        // gives it; `n` alone is just that day. `point.max(6)` rather than a
        // bare `6` for the one case where `n` is already past it: `7/k` is
        // Sunday spelled the second way, and a `7..=6` range would be reported
        // as running backwards, which is not the operator's mistake.
        if step.is_some() {
            (point, point.max(6))
        } else {
            (point, point)
        }
    };
    if low > high {
        return Err(format!(
            "day-of-week range '{base}' runs backwards; write it as two items instead"
        ));
    }

    let step = step.unwrap_or(1);
    let mut days = Vec::new();
    let mut day = low;
    while day <= high {
        days.push(day);
        day = day.saturating_add(step);
    }
    Ok(days)
}

/// One crontab day-of-week token: an ordinal in 0-7, or a day name.
///
/// The names are the crate's own table, copied rather than delegated to,
/// because a name inside a range has to be resolved here for the set expansion
/// above to see it. A spelling the crate accepts and this does not would be a
/// narrowing, so the list is its list.
fn day_ordinal(token: &str) -> Result<u32, String> {
    if let Ok(ordinal) = token.parse::<u32>() {
        return if ordinal <= 7 {
            Ok(ordinal)
        } else {
            Err(format!(
                "day-of-week '{token}' is out of range; a crontab allows 0-7, \
                 where 0 and 7 are both Sunday"
            ))
        };
    }
    match token.to_ascii_lowercase().as_str() {
        "sun" | "sunday" => Ok(0),
        "mon" | "monday" => Ok(1),
        "tue" | "tues" | "tuesday" => Ok(2),
        "wed" | "wednesday" => Ok(3),
        "thu" | "thurs" | "thursday" => Ok(4),
        "fri" | "friday" => Ok(5),
        "sat" | "saturday" => Ok(6),
        _ => Err(format!("'{token}' is not a day of the week")),
    }
}

/// `time` in, `chrono` out, at whole-second resolution.
///
/// The `cron` crate is built on `chrono` and this gear is built on `time`, so
/// one of the two boundaries has to exist. It is here, in these two conversion
/// functions, so that nothing chrono-shaped appears in a signature this module
/// exports.
///
/// **The zero nanoseconds here are not what makes a due time a whole second** —
/// [`from_chrono`] is. Preserving the sub-second part here instead leaves this
/// crate's whole suite green, which was measured rather than assumed, and the
/// reason it can is that `includes` compares whole seconds and the backward step
/// is reached only when `includes` was false — the case where both readings of
/// the boundary land on the same occurrence. It is written this way so both ends
/// of the boundary state the same resolution, not because a test can tell.
///
/// `None` only for an instant outside `chrono`'s range, which is wider than
/// `time`'s — so unreachable from an [`OffsetDateTime`], and handled rather than
/// unwrapped because "unreachable" is a claim about today's two crate versions.
fn to_chrono(instant: OffsetDateTime) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(instant.unix_timestamp(), 0)
}

/// The other direction, and **the truncation that is observable**: every due
/// time this module returns passes through here, so every one of them is a whole
/// second.
///
/// `None` for an instant `time` cannot represent; the `cron` crate's years stop
/// at 2100, so no occurrence it yields is one.
fn from_chrono(instant: DateTime<Utc>) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(instant.timestamp()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn hourly() -> &'static str {
        "0 * * * *"
    }

    #[test]
    fn a_valid_expression_parses() {
        assert!(parse_cron(hourly()).is_ok());
        assert!(parse_cron("*/15 * * * *").is_ok());
        assert!(parse_cron("30 2 * * 1").is_ok());
    }

    #[test]
    fn an_invalid_expression_is_a_validation_error() {
        for bad in ["", "not a cron", "* * * *", "99 * * * *", "* * * * * * *"] {
            assert!(parse_cron(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    /// A schedule that has never fired: its first outstanding tick is the most
    /// recent due time at or before `now`, not the next one in the future.
    #[test]
    fn a_never_fired_schedule_is_due_at_the_most_recent_past_occurrence() {
        let due = next_due(hourly(), None, datetime!(2026-08-13 12:30 UTC)).unwrap();
        assert_eq!(due, Some(datetime!(2026-08-13 12:00 UTC)));
    }

    #[test]
    fn a_schedule_fired_at_its_latest_due_time_is_not_due_again() {
        let due = next_due(
            hourly(),
            Some(datetime!(2026-08-13 12:00 UTC)),
            datetime!(2026-08-13 12:30 UTC),
        )
        .unwrap();
        assert_eq!(due, None);
    }

    #[test]
    fn a_schedule_becomes_due_again_at_the_next_occurrence() {
        let due = next_due(
            hourly(),
            Some(datetime!(2026-08-13 12:00 UTC)),
            datetime!(2026-08-13 13:00 UTC),
        )
        .unwrap();
        assert_eq!(due, Some(datetime!(2026-08-13 13:00 UTC)));
    }

    /// The catch-up policy (Step 1): after a long outage only the MOST RECENT
    /// due time fires. Back-filling six hours of an hourly destructive suite
    /// onto one platform is the failure this prevents.
    #[test]
    fn a_long_outage_fires_only_the_most_recent_due_time() {
        let due = next_due(
            hourly(),
            Some(datetime!(2026-08-13 06:00 UTC)),
            datetime!(2026-08-13 12:30 UTC),
        )
        .unwrap();
        assert_eq!(
            due,
            Some(datetime!(2026-08-13 12:00 UTC)),
            "skipped occurrences are never back-filled"
        );
    }

    /// And the skipped ones are enumerable, so an operator can be told which
    /// runs did not happen rather than being left to infer it.
    #[test]
    fn skipped_occurrences_are_reportable() {
        let skipped = skipped_since(
            hourly(),
            datetime!(2026-08-13 06:00 UTC),
            datetime!(2026-08-13 12:30 UTC),
        )
        .unwrap();
        assert_eq!(
            skipped,
            vec![
                datetime!(2026-08-13 07:00 UTC),
                datetime!(2026-08-13 08:00 UTC),
                datetime!(2026-08-13 09:00 UTC),
                datetime!(2026-08-13 10:00 UTC),
                datetime!(2026-08-13 11:00 UTC),
            ],
            "the most recent due time is fired, not skipped, so it is excluded"
        );
    }

    /// `now` exactly on an occurrence boundary is due, not pending — otherwise
    /// a tick that lands precisely on the minute silently waits a full period.
    #[test]
    fn an_occurrence_exactly_at_now_is_due() {
        let due = next_due(hourly(), None, datetime!(2026-08-13 12:00 UTC)).unwrap();
        assert_eq!(due, Some(datetime!(2026-08-13 12:00 UTC)));
    }

    /// Two identical calls agree.
    ///
    /// **This does not pin purity, and the plan that specified it said it did.**
    /// Two calls microseconds apart cannot detect a clock read: a `now()`
    /// consulted by both returns the same answer to both. Measured, not
    /// argued — a `chrono::Utc::now()` inserted into [`next_due`] and used to
    /// shift `now` by an hour on odd seconds passes this test every time, and
    /// is caught only by
    /// [`the_same_arguments_answer_the_same_whenever_they_are_asked`]. What
    /// this one is worth keeping for is the cheap half: the answer does not
    /// change between two calls, which a memoisation bug or an accidental
    /// `&mut self` would break.
    #[test]
    fn evaluation_is_pure_and_repeatable() {
        let args = (
            hourly(),
            Some(datetime!(2026-08-13 11:00 UTC)),
            datetime!(2026-08-13 12:30 UTC),
        );
        let first = next_due(args.0, args.1, args.2).unwrap();
        let second = next_due(args.0, args.1, args.2).unwrap();
        assert_eq!(first, second);
    }

    /// The evaluator must not read a clock, and this is what can tell.
    ///
    /// The load-bearing claim of this module is that every instance computes
    /// the same `due_at` for the same schedule, because the claim's unique
    /// index deduplicates *equal* due times and nothing else. A hidden `now()`
    /// breaks that between replicas whose clocks differ, and it breaks it
    /// invisibly.
    ///
    /// Two instants decades apart in one test are what discriminate: a wall
    /// clock is near one of them and nowhere near the other, so any answer
    /// derived from it is wrong for at least one. `chrono::Utc` is in scope in
    /// this module — it has to be, for the conversions — so the mistake is one
    /// keystroke away for a future editor, which is the reason this exists as a
    /// test rather than as a sentence in the header.
    ///
    /// **What it catches, measured over eight runs each.** A clock read that
    /// always happens: 8/8, both when `now` is ignored outright and when it is
    /// shifted by a clock-derived amount. A clock read *gated on a coin flip* —
    /// the review's `Utc::now().timestamp() % 2` mutant, which shifts by an hour
    /// only on odd seconds: **4/8**, against 0/8 for
    /// [`evaluation_is_pure_and_repeatable`]. That is a real limit and not a
    /// fixable one: half of that mutant's executions are the correct function,
    /// so no single deterministic run can distinguish it, and the only cures are
    /// sleeping past a second boundary or running the suite twice. CI runs it on
    /// every commit, which is what turns 4/8 into "caught soon" rather than
    /// "caught never" — but it is not a guarantee, and this paragraph exists so
    /// nobody reads it as one.
    #[test]
    fn the_same_arguments_answer_the_same_whenever_they_are_asked() {
        assert_eq!(
            next_due(hourly(), None, datetime!(1999-01-01 00:30 UTC)).unwrap(),
            Some(datetime!(1999-01-01 00:00 UTC)),
            "an instant decades in the past answers about that instant"
        );
        assert_eq!(
            next_due(hourly(), None, datetime!(2098-06-15 23:45 UTC)).unwrap(),
            Some(datetime!(2098-06-15 23:00 UTC)),
            "and one decades in the future answers about that one"
        );

        // The same discrimination through the other two entry points, since a
        // clock could be read in either of them just as easily.
        assert_eq!(
            skipped_since(
                hourly(),
                datetime!(1999-01-01 00:00 UTC),
                datetime!(1999-01-01 03:30 UTC),
            )
            .unwrap(),
            vec![
                datetime!(1999-01-01 01:00 UTC),
                datetime!(1999-01-01 02:00 UTC),
            ]
        );
        assert_eq!(
            parse_cron(hourly())
                .unwrap()
                .due_at_or_before(datetime!(2098-06-15 23:45 UTC)),
            Some(datetime!(2098-06-15 23:00 UTC))
        );
    }

    /// A schedule whose `last_fired` mark is in the future — a clock stepping
    /// backwards, or a restored backup — must not fire, and must not panic.
    #[test]
    fn a_last_fired_mark_in_the_future_yields_nothing() {
        let due = next_due(
            hourly(),
            Some(datetime!(2026-08-14 12:00 UTC)),
            datetime!(2026-08-13 12:30 UTC),
        )
        .unwrap();
        assert_eq!(due, None);
    }

    /// Everything is UTC. A schedule is not given a timezone here, so this
    /// pins that decision as a test rather than leaving it to be discovered.
    #[test]
    fn all_evaluation_is_utc() {
        let due = next_due("0 0 * * *", None, datetime!(2026-08-13 00:30 UTC)).unwrap();
        assert_eq!(due, Some(datetime!(2026-08-13 00:00 UTC)));
    }

    // ---------- the decisions this module had to make on its own ----------

    /// The other half of "everything is UTC", which the test above cannot
    /// reach: an input carrying a non-UTC offset is evaluated at the instant it
    /// names, and the answer comes back in UTC. Reading the wall-clock fields
    /// instead would make this 14:00+02:00.
    #[test]
    fn a_non_utc_input_is_evaluated_at_its_utc_instant() {
        let due = next_due(hourly(), None, datetime!(2026-08-13 14:30 +2)).unwrap();
        assert_eq!(due, Some(datetime!(2026-08-13 12:00 UTC)));
        assert_eq!(due.unwrap().offset(), time::UtcOffset::UTC);
    }

    /// A due time is an occurrence, never the instant the evaluator happened to
    /// be called at. `due_at` is the claim's unique key, so a due time carrying
    /// microseconds means two instances claim two different rows for one
    /// occurrence and the run fires twice.
    ///
    /// What this pins is the whole-second boundary, not one function: the
    /// conversion *out* is what enforces it, and mutating the conversion *in*
    /// alone leaves this green — see [`super::to_chrono`], which says so rather
    /// than claiming a guard it does not have.
    #[test]
    fn a_now_inside_an_occurrence_second_reports_the_occurrence_not_now() {
        let due = next_due(hourly(), None, datetime!(2026-08-13 12:00:00.999 UTC)).unwrap();
        assert_eq!(due, Some(datetime!(2026-08-13 12:00 UTC)));
        assert_eq!(due.unwrap().nanosecond(), 0);

        // Also from the backward step, which is a different code path from the
        // `includes` short-circuit above: 12:30:45.999 is not an occurrence, so
        // the answer comes from the iterator rather than from `now` itself.
        let due = next_due(hourly(), None, datetime!(2026-08-13 12:30:45.999 UTC)).unwrap();
        assert_eq!(due, Some(datetime!(2026-08-13 12:00 UTC)));
    }

    /// The same truncation on the other argument: a mark stored with sub-second
    /// precision must not leave its own occurrence looking outstanding.
    #[test]
    fn a_last_fired_mark_inside_the_occurrence_second_still_counts_as_fired() {
        let due = next_due(
            hourly(),
            Some(datetime!(2026-08-13 12:00:00.400 UTC)),
            datetime!(2026-08-13 12:30 UTC),
        )
        .unwrap();
        assert_eq!(due, None);
    }

    /// The `cron` crate numbers days of the week 1 = Sunday; crontab(5) numbers
    /// them 0 = Sunday, and crontab(5) is what this gear's stored expressions
    /// are read as. Untranslated, a numeric day either fires a day early or
    /// fails to parse.
    ///
    /// 2026-08-13 is a Thursday, so each assertion below names the weekday it
    /// expects by date.
    #[test]
    fn day_of_week_is_posix_numbered_not_the_crates_numbering() {
        // 0 is Sunday, and the crate rejects it outright without translation.
        let sunday = next_due("0 2 * * 0", None, datetime!(2026-08-13 12:00 UTC)).unwrap();
        assert_eq!(
            sunday,
            Some(datetime!(2026-08-09 02:00 UTC)),
            "0 must be the Sunday before, not a parse error"
        );

        // 1 is Monday. Untranslated the crate reads it as Sunday, 2026-08-09.
        let monday = next_due("0 2 * * 1", None, datetime!(2026-08-13 12:00 UTC)).unwrap();
        assert_eq!(monday, Some(datetime!(2026-08-10 02:00 UTC)));

        // 7 is POSIX's other spelling of Sunday.
        assert_eq!(
            next_due("0 2 * * 7", None, datetime!(2026-08-13 12:00 UTC)).unwrap(),
            sunday
        );

        // Names are resolved and renumbered here, not passed through: `MON` is
        // a crontab's 1, which this module renumbers to the crate's 2. See
        // `a_day_of_week_name_resolves_to_the_day_it_names` for all sixteen
        // spellings.
        assert_eq!(
            next_due("0 2 * * MON", None, datetime!(2026-08-13 12:00 UTC)).unwrap(),
            monday
        );

        // A range expands to its set and every day in it is renumbered; the
        // step selects members of that set and is not itself a day. Mon-Fri
        // every other day is Mon, Wed, Fri — 2026-08-12 is the Wednesday.
        assert_eq!(
            next_due("0 2 * * 1-5/2", None, datetime!(2026-08-13 12:00 UTC)).unwrap(),
            Some(datetime!(2026-08-12 02:00 UTC))
        );

        // `*/2` is Sun, Tue, Thu, Sat — the days a crontab's 0,2,4,6 names,
        // renumbered one by one. 2026-08-13 is the Thursday.
        assert_eq!(
            next_due("0 2 * * */2", None, datetime!(2026-08-13 12:00 UTC)).unwrap(),
            Some(datetime!(2026-08-13 02:00 UTC))
        );

        // A list, to pin that every item is translated and not just the first.
        assert_eq!(
            next_due("0 2 * * 0,1", None, datetime!(2026-08-13 12:00 UTC)).unwrap(),
            monday
        );
    }

    #[test]
    fn a_day_of_week_outside_the_posix_range_is_rejected() {
        // The last one also pins guard ordering: the day-of-week field is
        // judged whatever the day-of-month field says, so the message names the
        // field that is actually wrong.
        for bad in ["0 2 * * 8", "0 2 * * 1-9", "0 2 * * 0,9", "0 3 1 * 9"] {
            let err = parse_cron(bad).expect_err("an out-of-range day must be rejected");
            assert!(
                err.to_string().contains("0-7"),
                "the message must say what the range is: {err}"
            );
        }
    }

    /// A backwards range is refused by name rather than by the crate's
    /// contentless parse error, and a token that is neither a number nor a day
    /// says which token it was.
    #[test]
    fn a_malformed_day_of_week_says_what_is_wrong_with_it() {
        let err = parse_cron("0 2 * * 5-3").expect_err("a backwards range must be refused");
        assert!(err.to_string().contains("runs backwards"), "{err}");

        let err = parse_cron("0 2 * * funday").expect_err("a non-day must be refused");
        assert!(err.to_string().contains("funday"), "{err}");

        let err = parse_cron("0 2 * * 1-5/0").expect_err("a zero step must be refused");
        assert!(err.to_string().contains("positive"), "{err}");
    }

    /// A crontab fires when day-of-month *or* day-of-week matches; this crate
    /// fires only when both do. `0 3 1 * 1` is "the 1st, or any Monday".
    ///
    /// **Asserted in both directions**, because the union reverses with the
    /// direction of travel: the most recent occurrence at or before an instant
    /// is the *later* of the two forms' answers, and the next one after it is
    /// the *earlier*. A `min`/`max` swap is silent otherwise.
    ///
    /// Dates: 2026-08-13 is a Thursday, 2026-08-10 and 2026-08-03 are Mondays,
    /// 2026-08-01 is a Saturday and the month's 1st, 2026-07-27 is a Monday.
    #[test]
    fn either_day_field_matching_is_enough() {
        let both = "0 3 1 * 1";

        // Backward: the Monday is later than the 1st, so the Monday wins.
        assert_eq!(
            next_due(both, None, datetime!(2026-08-13 12:00 UTC)).unwrap(),
            Some(datetime!(2026-08-10 03:00 UTC)),
            "the most recent occurrence is the LATER of the two forms"
        );

        // Backward again with the other form winning — this is the assertion a
        // `max` -> `min` swap fails, since the Monday before is 2026-07-27.
        assert_eq!(
            next_due(both, None, datetime!(2026-08-02 12:00 UTC)).unwrap(),
            Some(datetime!(2026-08-01 03:00 UTC)),
            "the 1st is later than the Monday before it, so the 1st wins"
        );

        // Neither field alone gives those answers, which is what makes this a
        // union rather than a restatement of one form.
        assert_eq!(
            next_due("0 3 1 * *", None, datetime!(2026-08-13 12:00 UTC)).unwrap(),
            Some(datetime!(2026-08-01 03:00 UTC))
        );
        assert_eq!(
            next_due("0 3 * * 1", None, datetime!(2026-08-02 12:00 UTC)).unwrap(),
            Some(datetime!(2026-07-27 03:00 UTC))
        );

        // Forward, through `skipped_since`: 2026-08-01 (the 1st) and
        // 2026-08-03 (a Monday) both precede the 2026-08-10 that fires, and
        // they must come out oldest-first.
        assert_eq!(
            skipped_since(
                both,
                datetime!(2026-07-31 00:00 UTC),
                datetime!(2026-08-13 12:00 UTC),
            )
            .unwrap(),
            vec![
                datetime!(2026-08-01 03:00 UTC),
                datetime!(2026-08-03 03:00 UTC),
            ],
            "the union is enumerated in order, not one form then the other"
        );

        // An occurrence both forms match appears once. 2026-06-01 is a Monday
        // and the 1st; 2026-06-08 is the Monday that fires.
        assert_eq!(
            skipped_since(
                both,
                datetime!(2026-05-31 00:00 UTC),
                datetime!(2026-06-08 12:00 UTC),
            )
            .unwrap(),
            vec![datetime!(2026-06-01 03:00 UTC)],
            "a day matching both fields is one occurrence, not two"
        );

        // Inclusive at `now` for the union too, by way of the day-of-week form.
        assert_eq!(
            next_due(both, None, datetime!(2026-08-10 03:00 UTC)).unwrap(),
            Some(datetime!(2026-08-10 03:00 UTC))
        );
    }

    /// A field that spans everything but is not *written* as a star is still a
    /// restriction, so it takes the union path: `1-31` with a Monday is every
    /// day, not Mondays. A star form does not, so `*/1` with a Monday is
    /// Mondays. The distinction is where a crontab draws it, and reading it the
    /// other way silently turns a union into an intersection.
    #[test]
    fn a_full_range_is_still_a_restriction_but_a_star_form_is_not() {
        // 2026-08-12 is a Wednesday: matched by `1-31`, not by Monday.
        assert_eq!(
            next_due("0 3 1-31 * 1", None, datetime!(2026-08-12 12:00 UTC)).unwrap(),
            Some(datetime!(2026-08-12 03:00 UTC)),
            "1-31 is a restriction, so the union applies and every day matches"
        );
        assert_eq!(
            next_due("0 3 */1 * 1", None, datetime!(2026-08-12 12:00 UTC)).unwrap(),
            Some(datetime!(2026-08-10 03:00 UTC)),
            "a star form is unrestricted, so only the Monday matches"
        );
    }

    /// A day field beginning with a star is unrestricted whatever follows it,
    /// so the either-matches rule does not apply to it.
    ///
    /// This is the shape where crontab(5)'s prose and Vixie's implementation
    /// disagree — the prose tests the field against `*`, the implementation
    /// tests its first character — and where this module used to follow one
    /// reference for `*/2` and the other for `*,1`. See [`super::is_star_form`]
    /// for which is followed and why. A leading-star list is also **not** among
    /// the shapes the differential fuzz covered, so this is the only thing
    /// checking it.
    ///
    /// Dates: 2026-08-13 is a Thursday, 2026-08-10 a Monday, 2026-08-01 a
    /// Saturday and the month's 1st.
    #[test]
    fn a_field_beginning_with_a_star_is_unrestricted_whatever_follows() {
        let thursday_noon = datetime!(2026-08-13 12:00 UTC);

        // day-of-week `*/2` and `*,1` are both unrestricted, so each intersects
        // with the 1st rather than unioning with it. 2026-08-01 is a Saturday,
        // which `*/2` (Sun, Tue, Thu, Sat) contains and `*,1` contains as part
        // of its star.
        for day_of_week in ["*/2", "*,1", "*,MON"] {
            assert_eq!(
                next_due(&format!("0 3 1 * {day_of_week}"), None, thursday_noon).unwrap(),
                Some(datetime!(2026-08-01 03:00 UTC)),
                "day-of-week {day_of_week} must not trigger the union"
            );
        }

        // The same in the day-of-month field: unrestricted, so only the Monday
        // constrains anything.
        for day_of_month in ["*/1", "*,1", "*,15"] {
            assert_eq!(
                next_due(&format!("0 3 {day_of_month} * 1"), None, thursday_noon).unwrap(),
                Some(datetime!(2026-08-10 03:00 UTC)),
                "day-of-month {day_of_month} must not trigger the union"
            );
        }

        // A star that is not first is not a star form, so the union applies and
        // every day matches. This is the boundary of the rule, not a restatement
        // of it: 2026-08-13 is neither the 1st nor a Monday.
        assert_eq!(
            next_due("0 3 1,* * 1", None, thursday_noon).unwrap(),
            Some(datetime!(2026-08-13 03:00 UTC)),
            "`1,*` does not begin with a star, so it restricts and the union applies"
        );
    }

    /// The bug this module shipped once: a day-of-week range whose endpoint is
    /// `7` renumbers *below* the days before it, so rewriting endpoints in
    /// place turns `0-7` — every day — into `1-1`, Sunday alone, and rejects
    /// `5-7` outright with a message claiming it is not valid cron.
    ///
    /// 2026-08-13 is a Thursday; 2026-08-09 is the Sunday before it and
    /// 2026-08-14 is the Friday after.
    #[test]
    fn a_day_of_week_range_that_contains_seven_keeps_every_day_in_it() {
        let thursday_noon = datetime!(2026-08-13 12:00 UTC);

        // `0-7` is every day, so the Thursday itself is due.
        assert_eq!(
            next_due("0 2 * * 0-7", None, thursday_noon).unwrap(),
            Some(datetime!(2026-08-13 02:00 UTC)),
            "0-7 is every day, not Sunday alone"
        );
        assert_eq!(
            next_due("0 2 * * 1-7", None, thursday_noon).unwrap(),
            Some(datetime!(2026-08-13 02:00 UTC)),
            "1-7 is Monday through Sunday, which is also every day"
        );

        // `5-7` is Fri, Sat, Sun and `6-7` is Sat, Sun — different sets, so a
        // Friday tells them apart. Both were rejected before.
        let friday_noon = datetime!(2026-08-14 12:00 UTC);
        assert_eq!(
            next_due("0 2 * * 5-7", None, friday_noon).unwrap(),
            Some(datetime!(2026-08-14 02:00 UTC)),
            "5-7 includes Friday"
        );
        assert_eq!(
            next_due("0 2 * * 6-7", None, friday_noon).unwrap(),
            Some(datetime!(2026-08-09 02:00 UTC)),
            "6-7 is Saturday and Sunday, so the Sunday before is the answer"
        );

        // A step over a range containing 7: 0-7/2 is Sun, Tue, Thu, Sat.
        assert_eq!(
            next_due("0 2 * * 0-7/2", None, thursday_noon).unwrap(),
            Some(datetime!(2026-08-13 02:00 UTC))
        );
        // And a list mixing a 7-range with a name.
        assert_eq!(
            next_due("0 2 * * 6-7,WED", None, thursday_noon).unwrap(),
            Some(datetime!(2026-08-12 02:00 UTC)),
            "the Wednesday, since Sat/Sun are further back"
        );
    }

    /// Every spelling the crate's own table accepts, each pinned against the
    /// crontab number for the same day.
    ///
    /// The table in [`super::day_ordinal`] is a **copy** of
    /// `cron-0.17.0/src/time_unit/days_of_week.rs`'s `DAY_OF_WEEK_MAP`, and a
    /// copy is the kind of thing that loses an entry without anything noticing:
    /// before this test, deleting `"tues"` left the suite green, because only
    /// three of the sixteen spellings were exercised anywhere.
    ///
    /// Comparing a name against its *number* rather than against a literal date
    /// is what makes this a check of the mapping: numbers are parsed, not
    /// looked up, so the two sides cannot be wrong together.
    #[test]
    fn a_day_of_week_name_resolves_to_the_day_it_names() {
        let sunday_noon = datetime!(2026-08-16 12:00 UTC);
        let by_number = |day: u32| {
            next_due(&format!("0 2 * * {day}"), None, sunday_noon)
                .unwrap()
                .expect("every weekday has occurred in the past week")
        };

        for (spelling, day) in [
            ("sun", 0),
            ("sunday", 0),
            ("mon", 1),
            ("monday", 1),
            ("tue", 2),
            ("tues", 2),
            ("tuesday", 2),
            ("wed", 3),
            ("wednesday", 3),
            ("thu", 4),
            ("thurs", 4),
            ("thursday", 4),
            ("fri", 5),
            ("friday", 5),
            ("sat", 6),
            ("saturday", 6),
        ] {
            for written in [spelling.to_owned(), spelling.to_uppercase()] {
                let named = next_due(&format!("0 2 * * {written}"), None, sunday_noon)
                    .unwrap_or_else(|e| panic!("{written} must be a day of the week: {e}"));
                assert_eq!(named, Some(by_number(day)), "{written} must mean day {day}");
            }
        }

        // The comparison is only worth anything if the seven numbers name seven
        // different days, which they do in the week before a Sunday noon.
        let mut distinct: Vec<_> = (0..7).map(by_number).collect();
        distinct.dedup();
        assert_eq!(distinct.len(), 7, "the seven numbers must name seven days");
    }

    /// `5/2` is Friday alone, not Friday and Sunday: a step written on a bare
    /// day runs from that day to the **end of the week**, and the week ends at
    /// Saturday.
    ///
    /// **This is our reading, not a quoted one.** crontab(5) documents steps on
    /// ranges; a step on a bare day is an extension that implementations vary
    /// on, and no external implementation was measured for it — not Argo's, not
    /// Vixie's. It is pinned here because [`super::expand_day_item`] carries a
    /// six-line rationale for the choice and nothing was checking that the code
    /// still made it: changing its `6` to a `7` left the whole suite green.
    ///
    /// 2026-08-16 is a Sunday; 2026-08-14 is the Friday before it.
    #[test]
    fn a_step_on_a_bare_day_runs_to_the_end_of_the_week_not_past_it() {
        let sunday_noon = datetime!(2026-08-16 12:00 UTC);

        // Friday, and *not* the Sunday that a week ending at 7 would add.
        assert_eq!(
            next_due("0 2 * * 5/2", None, sunday_noon).unwrap(),
            Some(datetime!(2026-08-14 02:00 UTC)),
            "5/2 is Friday alone; a week ending at 7 would make it Friday and Sunday"
        );
        // Mon, Wed, Fri — the same boundary from a different start.
        assert_eq!(
            next_due("0 2 * * 1/2", None, sunday_noon).unwrap(),
            Some(datetime!(2026-08-14 02:00 UTC)),
            "1/2 stops at Friday rather than reaching Sunday"
        );
        // `7` is already the last day a crontab can name, so a step on it is
        // just that day rather than a range that runs backwards.
        assert_eq!(
            next_due("0 2 * * 7/2", None, sunday_noon).unwrap(),
            Some(datetime!(2026-08-16 02:00 UTC)),
            "7/2 is Sunday, not a rejected backwards range"
        );
    }

    /// Names are resolved by this module rather than passed to the crate, so a
    /// range mixing a number and a name translates as one set instead of being
    /// half-shifted.
    #[test]
    fn a_day_of_week_range_may_mix_numbers_and_names() {
        // 1-FRI is Monday through Friday. 2026-08-09 is a Sunday, so the most
        // recent weekday before Sunday noon is the Friday, 2026-08-07.
        assert_eq!(
            next_due("0 2 * * 1-FRI", None, datetime!(2026-08-09 12:00 UTC)).unwrap(),
            Some(datetime!(2026-08-07 02:00 UTC))
        );
        assert_eq!(
            next_due("0 2 * * MON-5", None, datetime!(2026-08-09 12:00 UTC)).unwrap(),
            Some(datetime!(2026-08-07 02:00 UTC))
        );
    }

    /// The `@`-descriptors an operator could type against legacy, each pinned
    /// against the five-field expression it stands for.
    #[test]
    fn a_descriptor_expands_to_its_five_field_equivalent() {
        let now = datetime!(2026-08-13 12:34 UTC);
        for (descriptor, equivalent) in [
            ("@hourly", "0 * * * *"),
            ("@daily", "0 0 * * *"),
            ("@midnight", "0 0 * * *"),
            ("@weekly", "0 0 * * 0"),
            ("@monthly", "0 0 1 * *"),
            ("@yearly", "0 0 1 1 *"),
            ("@annually", "0 0 1 1 *"),
        ] {
            assert_eq!(
                next_due(descriptor, None, now).unwrap(),
                next_due(equivalent, None, now).unwrap(),
                "{descriptor} must mean {equivalent}"
            );
        }
        // Case is not a technicality worth refusing somebody over.
        assert_eq!(
            next_due("@Daily", None, now).unwrap(),
            next_due("0 0 * * *", None, now).unwrap()
        );
        // And the expansion is real rather than an alias that happens to agree:
        // @weekly is Sunday, and 2026-08-09 is the Sunday before `now`.
        assert_eq!(
            next_due("@weekly", None, now).unwrap(),
            Some(datetime!(2026-08-09 00:00 UTC))
        );
    }

    /// `@reboot` has no due time to claim, and the message has to say so rather
    /// than complain about a field count.
    #[test]
    fn reboot_and_unknown_descriptors_are_refused_by_name() {
        let err = parse_cron("@reboot").expect_err("@reboot must be refused");
        let rendered = err.to_string();
        assert!(rendered.contains("no due time"), "{rendered}");
        assert!(
            rendered.contains("@daily"),
            "the message must list what works"
        );

        for unknown in ["@wekly", "@every 5m", "@"] {
            let err = parse_cron(unknown).expect_err("an unknown descriptor must be refused");
            assert!(
                err.to_string().contains("descriptor"),
                "{unknown:?} must be refused as a descriptor, not by field count: {err}"
            );
        }
    }

    /// The five-field contract, from both sides. The crate itself wants six or
    /// seven, so an unnormalised five-field expression would be rejected and a
    /// six-field one would be silently accepted with its fields shifted by one
    /// — `0 30 2 * * *` read as a seconds field would fire every minute.
    #[test]
    fn only_five_fields_are_accepted() {
        assert!(parse_cron("0 * * * *").is_ok());
        for wrong in ["0 * * *", "0 0 * * * *", "   "] {
            let err = parse_cron(wrong).expect_err("only a five-field expression may be accepted");
            assert!(
                err.to_string().contains("expected 5 fields"),
                "{wrong:?} must be refused for its field count: {err}"
            );
        }
    }

    /// The seconds field the normalisation injects is `0`, not `*`. With `*` an
    /// hourly schedule would have sixty occurrences an hour, and `next_due`
    /// would report a different due time every second.
    #[test]
    fn the_injected_seconds_field_is_zero() {
        let schedule = parse_cron(hourly()).unwrap();
        assert_eq!(
            schedule.due_at_or_before(datetime!(2026-08-13 12:00:30 UTC)),
            Some(datetime!(2026-08-13 12:00:00 UTC))
        );
    }

    /// A schedule with no occurrence at or before `now` is not due. This is
    /// where the `None` arm of `due_at_or_before` is exercised: an expression
    /// with occurrences going back to 1970 can never reach it, and every other
    /// `None` in this suite comes from the `last_fired` comparison instead.
    #[test]
    fn a_schedule_whose_first_occurrence_is_still_ahead_is_not_due() {
        // February 29th, and 2026 is not a leap year: the previous occurrence
        // is 2024-02-29 and the next is 2028-02-29.
        assert_eq!(
            next_due("0 0 29 2 *", None, datetime!(2026-08-13 12:00 UTC)).unwrap(),
            Some(datetime!(2024-02-29 00:00 UTC)),
            "a rare expression still answers with its most recent occurrence"
        );
        // The crate's years start at 1970, so this one has no past occurrence
        // at all.
        assert_eq!(
            next_due(
                "0 0 1 1 *",
                None,
                datetime!(1970-01-01 00:00 UTC).replace_year(1969).unwrap()
            )
            .unwrap(),
            None
        );
    }

    /// The parser bounds its input, which nothing else does.
    ///
    /// Three consequences, all measured on the unguarded version and all closed
    /// by one check: a 500 KB expression of repeated `0-59` lists parsed
    /// *successfully* in 97 ms and would be re-parsed on every tick; a rejected
    /// one produced a `DomainError` whose `Display` was half a megabyte, which
    /// [`DomainError::disclosable`] classifies as returnable to the caller and
    /// loggable; and a 607-byte expression parsed and would then have failed at
    /// the `INSERT` against a `VARCHAR(255)` column, which is a 500 where the
    /// caller deserves a 400.
    ///
    /// That last one **cannot be reproduced by any test in this crate**: the
    /// `SQLite` tier declares the column `TEXT`. This test stands in for it.
    #[test]
    fn an_over_long_expression_is_refused_without_being_echoed() {
        // Valid cron, and far too long to store.
        let long = vec!["0-59"; 100_000].join(",");
        let expression = format!("{long} * * * *");
        let err = parse_cron(&expression).expect_err("an over-long expression must be refused");
        let rendered = err.to_string();
        assert!(
            rendered.contains("255"),
            "the message must state the limit: {rendered}"
        );
        assert!(
            rendered.len() < 200,
            "the expression must not be echoed whole: {} bytes",
            rendered.len()
        );

        // The boundary, both sides, against the column width rather than a
        // number invented here.
        let filler = "0".repeat(MAX_EXPRESSION_LEN - "  * * * *".len() - 1);
        let at_limit = format!("{filler},1 * * * *");
        assert_eq!(at_limit.chars().count(), MAX_EXPRESSION_LEN);
        assert!(
            parse_cron(&at_limit).is_ok(),
            "an expression exactly at the limit must still parse"
        );
        assert!(parse_cron(&format!("0{at_limit}")).is_err());

        // Length is judged before anything else, so a long *invalid* expression
        // is also not echoed.
        let err = parse_cron(&"9".repeat(500_000)).expect_err("still refused");
        assert!(err.to_string().len() < 200, "{}", err.to_string().len());
    }

    /// `skipped_since` bounds what it allocates. Two days of a per-minute
    /// schedule is more occurrences than the cap, and the answer is the oldest
    /// of them with nothing marking it as partial — which is why the doc says
    /// so and why this test exists rather than an assertion that it is
    /// complete.
    #[test]
    fn a_very_long_gap_is_truncated_at_the_reporting_cap() {
        let skipped = skipped_since(
            "* * * * *",
            datetime!(2026-08-11 12:00 UTC),
            datetime!(2026-08-13 12:00 UTC),
        )
        .unwrap();
        assert_eq!(skipped.len(), MAX_SKIPPED_REPORTED);
        assert_eq!(
            skipped.first(),
            Some(&datetime!(2026-08-11 12:01 UTC)),
            "truncation keeps the oldest, not the newest"
        );
    }

    /// Nothing was skipped when nothing was missed, and the boundary at `since`
    /// is exclusive so a mark is never reported as its own skip.
    #[test]
    fn a_schedule_that_missed_nothing_reports_nothing() {
        assert!(
            skipped_since(
                hourly(),
                datetime!(2026-08-13 11:00 UTC),
                datetime!(2026-08-13 12:30 UTC),
            )
            .unwrap()
            .is_empty(),
            "11:00 is the mark and 12:00 is what fires, so nothing is skipped"
        );
        assert!(
            skipped_since(
                hourly(),
                datetime!(2026-08-13 12:30 UTC),
                datetime!(2026-08-13 12:45 UTC),
            )
            .unwrap()
            .is_empty(),
            "no occurrence lies in the window at all"
        );
    }

    /// Both evaluators reject what `parse_cron` rejects, rather than one of
    /// them treating an unparseable expression as "nothing due".
    #[test]
    fn an_unparseable_expression_is_an_error_from_every_entry_point() {
        let now = datetime!(2026-08-13 12:30 UTC);
        assert!(matches!(
            next_due("nope", None, now),
            Err(DomainError::InvalidCron { .. })
        ));
        assert!(matches!(
            skipped_since("nope", now, now),
            Err(DomainError::InvalidCron { .. })
        ));
    }

    /// Every field except day-of-week fails inside the crate, whose error text
    /// is empty of diagnosis, so this module supplies the one thing it honestly
    /// knows: which five fields there are and what each may hold. A zero
    /// day-of-month is the common version of this mistake; the Quartz spellings
    /// are what somebody migrating from a scheduler that has them will try.
    #[test]
    fn a_refusal_names_the_fields_and_their_ranges() {
        for bad in [
            "0 2 0 * *",
            "0 2 * */0 *",
            "0 2 -1 * *",
            "0 2 L * *",
            "0 2 1W * *",
        ] {
            let err = parse_cron(bad).expect_err("must be refused");
            let rendered = err.to_string();
            assert!(
                rendered.contains("day-of-month 1-31") && rendered.contains("minute 0-59"),
                "{bad:?} must be told what the fields are: {rendered}"
            );
        }
    }

    /// The error names the expression the operator typed, not the six-field
    /// form this module builds out of it.
    #[test]
    fn the_error_quotes_what_the_operator_wrote() {
        let err = parse_cron("99 * * * *").expect_err("minute 99 must be rejected");
        let rendered = err.to_string();
        assert!(rendered.contains("99 * * * *"), "{rendered}");
        assert!(
            !rendered.contains("0 99 * * * *"),
            "the normalised form must not surface: {rendered}"
        );
    }
}
