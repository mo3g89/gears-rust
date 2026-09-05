//! Today, as the analytics cores see it.
//!
//! # Why a port at all
//!
//! Every window in [`crate::domain::analytics`] is anchored on the current
//! date. `recent_days` (`manager/src/routes/analytics.rs:2077-2082`) opens with
//! `let today = Utc::now().date_naive();` and `build_flaky` (`:1657`) computes
//! its cutoff the same way, so in legacy the day axis of the heatmap, the trend
//! and the flaky window are all read straight off the system clock deep inside
//! the fold. A core that does that cannot be pinned by a test: the heatmap
//! assertions would pass on the day they were written and fail on a date
//! boundary.
//!
//! The production adapter is one line
//! ([`crate::infra::clock::SystemClock`]); the test adapter is a constant
//! (`domain::service::test_support::FixedClock`, which is `#[cfg(test)]` and so
//! is named in prose rather than linked), and that is what makes the heatmap,
//! trend and flaky assertions stable.
//!
//! # The cores take a [`Date`], not this trait, and that is the whole point
//!
//! Task 22's brief asks for the clock to be threaded "into the aggregate
//! functions as a parameter, not as struct state — the cores stay pure functions
//! and the parameter is the only thing that makes them so". A fold that takes
//! `&dyn Clock` is *deterministic under a fake* but it is not a pure function,
//! and this crate already settled the shape one Phase earlier:
//! `domain::service::dashboard`'s private `daily_points` takes `today: Date` and
//! [`DashboardService::stats`](crate::domain::service::dashboard::DashboardService::stats)
//! reads `OffsetDateTime::now_utc()` **once** (`domain/service/dashboard.rs:485-486`)
//! and binds every window from it. So:
//!
//! * the **service** holds the clock and reads it once per request;
//! * the **folds** take the resulting [`Date`].
//!
//! Reading the clock once is not tidiness. `build_heatmap`, `build_trend` and
//! `build_flaky` each call `Utc::now()` in legacy, so a request that crosses
//! midnight between two of them gets a heatmap whose last column is one day
//! behind the trend's last point. `domain::service::dashboard`'s header makes the
//! same argument for the dashboard's three statements ("every number in one
//! payload shares one instant"). This is a substitution rather than a
//! translation, and it is recorded here rather than discovered later.
//!
//! # Why not `OffsetDateTime::now_utc()` in the service, as the dashboard does
//!
//! Because the analytics service's *output* is a day axis, where the
//! dashboard's is mostly counters. `DashboardService::stats` calls the system
//! clock directly and its tests pay for it: `dashboard_tests`' `seed` stamps its
//! rows with `now()` and sums over the whole trend rather than reading its last
//! point, precisely because it cannot say what "today" is (that fixture's own
//! doc, "Why the real clock here and a fixed date in the folds above", records
//! the workaround). Task 25's analytics service returns `days` labelled cells
//! per test; there is no sum to fall back on, so the service tier needs the
//! clock to be substitutable and not merely worked around.
//!
//! Retrofitting the dashboard onto this port is **not** part of Task 22 — it
//! would change a shipped service's constructor for no behaviour change. It is
//! noted here as the obvious second caller; Task 25b added the *first*
//! ([`crate::domain::service::analytics::AnalyticsService`]) and left the
//! dashboard alone for that reason.
//!
//! # One method, and no `now()`
//!
//! [`Clock::today`] is the whole port. Nothing in this gear needs a
//! substitutable *instant*: the two places that mint one — the repository's
//! `created_at` stamps and the reconcile watermark — are deliberately the
//! database's and the sweep's own clocks, and `infra::storage::results_sea_repo`
//! brackets its assertions between two readings of the real clock rather than
//! freezing it. A `now()` here would be a second convention for the same
//! question with no caller, which is the shape
//! [`super`]'s header refuses.

use time::Date;

/// The current UTC date, as the analytics windows anchor on it.
pub trait Clock: Send + Sync {
    /// Today in **UTC**, never in a local offset.
    ///
    /// Legacy's `Utc::now().date_naive()` (`analytics.rs:2078`, `:1657`).
    /// `domain::service::dashboard`'s `daily_points` makes the same choice for
    /// the same reason — legacy's `DATE(...)` is evaluated in the database
    /// session's timezone, which is a deployment setting rather than a decision,
    /// so UTC is fixed explicitly here and the same rows bucket the same way on
    /// every deployment.
    fn today(&self) -> Date;
}
