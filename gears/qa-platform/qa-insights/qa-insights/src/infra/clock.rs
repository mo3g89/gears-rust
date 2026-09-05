//! The production [`Clock`]: this process's UTC date.
//!
//! One line of behaviour, in `infra` rather than in `domain` because reading the
//! system clock is egress in exactly the sense the port exists to isolate — the
//! same reason the qa-runs reads live in [`super::clients`] and not next to
//! their port. `domain::ports::clock`'s header carries the argument for the port
//! itself.
//!
//! **Wired by Task 25b**, which authored the analytics service that holds it and
//! constructed this adapter in `gear::init` — one task rather than the two this
//! paragraph forecast, because Task 25 needed the service and the service needed
//! the clock. It shipped one phase earlier, with the port, so that the trait had
//! a production implementor rather than only a test double —
//! `domain::ports::mod`'s header records that a port with no adapter is "a guess
//! about a shape rather than a contract", and this was the cheapest possible way
//! to stop being one.

use time::{Date, OffsetDateTime};

use crate::domain::ports::Clock;

/// [`Clock`] over `OffsetDateTime::now_utc()`.
///
/// Legacy's `Utc::now().date_naive()` (`manager/src/routes/analytics.rs:2078`),
/// spelled in `time` rather than `chrono`. Unit struct: there is no
/// configuration to hold, and a constructor would only be a second way to write
/// `SystemClock`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn today(&self) -> Date {
        // `now_utc`, not `now_local`. The offset is not a deployment choice
        // here: `Clock::today`'s doc records why the analytics day axis is fixed
        // to UTC, and `now_local` is additionally fallible on this platform.
        OffsetDateTime::now_utc().date()
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, OffsetDateTime, SystemClock};

    /// The adapter reads the **real** clock, bracketed rather than compared to a
    /// literal.
    ///
    /// `infra::storage::results_sea_repo` makes the same choice for the same
    /// reason: `assert_eq!(SystemClock.today(), <a date>)` is a test that passes
    /// for one day, and pinning the clock instead would only assert that the
    /// fake is the fake. Two readings either side of the call bound it, and the
    /// bracket holds across midnight because it widens to two days rather than
    /// failing.
    ///
    /// What it catches: an adapter that returns a constant, one that reads a
    /// local offset far enough from UTC to cross a date line, and one that
    /// returns the epoch because a conversion silently defaulted.
    #[test]
    fn the_system_clock_reports_the_current_utc_date() {
        let before = OffsetDateTime::now_utc().date();
        let today = SystemClock.today();
        let after = OffsetDateTime::now_utc().date();

        assert!(
            today >= before && today <= after,
            "SystemClock::today must be this process's UTC date: got {today}, bracketed by {before}..={after}",
        );
    }
}
