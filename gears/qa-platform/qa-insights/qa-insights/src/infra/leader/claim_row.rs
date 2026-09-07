//! A claim-row elector: one holder per role, in the gear's own database.
//!
//! [`super`]'s header argues that election in this gear is an optimisation.
//! That argument is correct for
//! [`ROLE_RECONCILER`](super::ROLE_RECONCILER) and
//! [`ROLE_COLLECT`](super::ROLE_COLLECT), and this elector is not for them —
//! it exists for [`ROLE_JIRA_POLLER`](super::ROLE_JIRA_POLLER) alone, whose
//! effect is `RunsLauncher::launch_test`: a new run, not a converging write.
//! See that header's "The JIRA poller is the exception" for the whole
//! argument; what follows is how this implementation earns it.
//!
//! # The CAS, and why two acquirers cannot both win
//!
//! Acquiring is two conditional writes and **no read-then-write**, which is
//! the property the whole thing rests on: there is no window between a
//! decision and the write that acts on it, because the decision *is* the
//! write's `WHERE` clause.
//!
//! 1. `UPDATE qa_leader_claims SET holder = me, claimed_at = <db now>,
//!    expires_at = <db now> + ttl WHERE tenant_id = nil AND role = ? AND
//!    (expires_at <= <db now> OR holder = me)`. One row affected means the
//!    claim is ours. Zero means it is someone else's and still live.
//! 2. Only if that matched nothing: `INSERT` a row for this role. A peer that
//!    raced us between the two loses on `idx_qa_leader_claims_role`, which is
//!    reported as "not ours" rather than as an error — `NotifyRepository::
//!    claim_notification` reads a unique violation the same way and for the
//!    same reason (`infra::storage::notify_sea_repo`).
//!
//! **Two acquirers arriving together on an expired row are serialised by the
//! row lock the `UPDATE` takes.** The second waits for the first to commit and
//! then re-evaluates its own `WHERE` against the row the first wrote — under
//! `READ COMMITTED`, which is what these statements run in, Postgres
//! re-checks the predicate on the updated version rather than on the snapshot
//! it started from. By then `expires_at` is in the future and `holder` is the
//! winner, so the loser matches zero rows, falls through to the `INSERT`, and
//! loses again on the unique index. This is
//! `qa-environments/src/infra/storage/leases_sea_repo.rs`' CAS with the
//! guard changed from `version = expected` to "expired or mine": both turn
//! "did I win?" into a row count the database decided, and both turn the
//! first-write race into a unique violation.
//!
//! `INSERT` never writes a worker's clock into either timestamp. It writes the
//! Unix epoch and then runs step 1 against the row it just created — which
//! matches, since the epoch is expired — so the only values `claimed_at` and
//! `expires_at` ever hold come from `NOW()` on the database. `coord`'s lease
//! (`gears/bss/libs/coord/src/lease/manager.rs`) is where that trick is from,
//! and its reason is the one that matters here: a replica whose clock runs
//! slow would otherwise judge a live claim expired **and stamp that judgement
//! into its own `WHERE`**, stealing a claim someone still holds. Two leaders,
//! from a clock nobody was watching. Anchoring every comparison and every
//! write on the database's clock makes skew a false *negative* — a replica
//! that fails to take a claim it could have — which costs a poll interval and
//! never correctness.
//!
//! # A crashed holder does not keep the role
//!
//! The claim expires. [`ClaimRowElector::run_role`] renews it on a heartbeat
//! while the work runs, and a holder that stops renewing — because it died,
//! was partitioned, or lost its database — leaves a row whose `expires_at`
//! passes, at which point step 1 above matches it for the next replica that
//! contends. The window is bounded by [`DEFAULT_TTL`] and the losers' retry
//! cadence, not by anything an operator has to do.
//!
//! The reverse failure — a holder that is *still running the work* when its
//! claim lapses — is why the heartbeat cancels the term rather than only
//! logging. `run_role` hands the work a child of the caller's cancellation
//! token, so losing the claim fires exactly the same shutdown the gear fires,
//! and the ticker stops the way it already knows how to.
//!
//! **The residual, stated rather than implied: this is a claim row, not a
//! fencing token.** Between a renewal failing and the work observing the
//! cancelled token, work already in flight keeps running, and another replica
//! may by then hold the claim. The bound on that overlap is one iteration of
//! whatever the work does between token checks, and for `crate::gear`'s
//! `jira_poller_ticker` that is **one tenant's `poll_once`, not a whole
//! pass**: `jira_poll_pass` tests the token at the top of its per-tenant loop,
//! so a cancelled term stops at the next tenant boundary rather than at the
//! next tick. Closing the remainder needs a fence the *launch* checks, which
//! means a token on `RunsLauncher::launch_test`, in another gear. What this
//! elector removes is the steady-state duplication: two healthy replicas, both
//! polling, forever.
//!
//! `tests::a_stolen_claim_stops_the_term` is what holds that bound to
//! something demonstrated rather than argued — it is the test that fails if
//! `ClaimRowElector::end_term` ever stops firing, which would turn this
//! bounded overlap into an unbounded one with nothing going red.
//!
//! # Where this sits among the tree's other leases, counted rather than
//! # gestured at
//!
//! Three numbers, because they are three different sets and conflating them is
//! how a "fourth copy" note goes stale:
//!
//! * **Four copies of the [`LeaderElector`] trait** — qa-runs, chat-engine,
//!   mini-chat and this gear. [`super`]'s header names them and this file does
//!   not change that count.
//! * **Three implementations of it that actually elect** — chat-engine's and
//!   mini-chat's `K8sLeaseElector` (a Kubernetes `Lease`), and this one. The
//!   other three impls in the tree are `NoopLeaderElector`s.
//! * **Three DB-backed leases** — `account-management`'s `am_leases`,
//!   `gears/bss/libs/coord` (a shared crate: `LeaseManager`, the same DB-clock
//!   arithmetic, a renewal task, a guard), and this table. This is the first
//!   that is *both* a DB-backed lease and a `LeaderElector`, which is why
//!   reusing `coord` outright would still have left an adapter to write.
//!
//! `coord` is not used here for a second reason as well: it is a BSS-family
//! library and reaching across families for it is a bigger decision than this
//! fix owns. It and `am_leases` are named so that whoever consolidates them can
//! find this one.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sea_orm::sea_query::{Expr, SimpleExpr};
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_db::secure::{ScopeError, SecureDeleteExt, SecureUpdateExt};
use toolkit_security::AccessScope;
use tracing::{info, warn};
use uuid::Uuid;

use super::{LeaderElector, LeaderWorkFn};
use crate::domain::service::DbProvider;

/// The `tenant_id` every claim row this gear writes carries.
///
/// [`LeaderElector::run_role`] takes a role and no tenant, because a ticker
/// here holds its role for *every* tenant it then enumerates — the same "no
/// tenant to take one from" that makes `system_actor::for_ticker_enumeration`
/// nil-tenant (`domain::elevated`'s header). The column is in the unique index
/// regardless, so a per-tenant election later is a change to this file and not
/// to the schema.
///
/// This is a tenant-bound scope, not `AccessScope::allow_all()`: `elevated`'s
/// own doc restricts the unrestricted scope to reads, and every statement here
/// writes.
const CLAIM_TENANT: Uuid = Uuid::nil();

/// How long a claim stays valid without a renewal.
///
/// The upper bound on how long a role goes unheld after its holder dies, and
/// therefore on how late one JIRA poll runs. Generous next to
/// [`DEFAULT_RENEW_EVERY`] on purpose: three heartbeats may fail before the
/// claim actually lapses, so a database hiccup does not hand the role to
/// another replica while the first is still perfectly able to poll.
pub const DEFAULT_TTL: Duration = Duration::from_mins(1);

/// How often the holder pushes [`DEFAULT_TTL`] forward while its work runs.
pub const DEFAULT_RENEW_EVERY: Duration = Duration::from_secs(20);

/// How long a replica that lost waits before contending again.
///
/// This is what bounds the takeover delay together with [`DEFAULT_TTL`]: a
/// dead holder's claim lapses after the TTL and the next contender notices
/// within one of these.
pub const DEFAULT_RETRY_EVERY: Duration = Duration::from_secs(15);

/// The `qa_leader_claims` entity.
///
/// Deliberately **not** in [`crate::infra::storage::entity`], whose own header
/// defines that module as "one per table in `m20260818_000001_initial`" and
/// whose round-trip suite enumerates exactly those eleven. This table is this
/// elector's private mechanism — nothing else reads or writes it, and no SDK
/// type mirrors it — so it lives with the code that is the only reason it
/// exists. `coord`'s `coord_leases` entity sits next to its `LeaseManager` for
/// the same reason.
mod entity {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;
    use toolkit_db_macros::Scopable;
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "qa_leader_claims")]
    #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        /// The role name — `super::super::ROLE_JIRA_POLLER` today, and the
        /// only one this gear claims.
        pub role: String,
        /// The replica holding it. A fresh v4 per [`super::ClaimRowElector`],
        /// so a restarted process is a different holder and cannot renew the
        /// claim its predecessor left behind.
        pub holder: Uuid,
        /// When the current holder took it. Written by the database's clock;
        /// see this module's header.
        ///
        /// # Nothing decodes this field, and a future reader of it breaks on
        /// # `SQLite` only
        ///
        /// The declared type is what `SeaORM` *binds* with, and every write
        /// here is a `col_expr` — `NOW()` on Postgres, `datetime('now')` on
        /// `SQLite` — so the value that lands in the column is the database's
        /// own spelling, never a `time::OffsetDateTime` serialisation. On
        /// Postgres that is a native `TIMESTAMPTZ` and reads back fine. On
        /// `SQLite` the column is `TEXT` holding `2026-09-07 12:00:00`, which
        /// is **not** the RFC-3339 an `OffsetDateTime` decoder expects.
        ///
        /// So this is safe exactly as long as it stays true that no statement
        /// reads the row: [`super::ClaimRowElector`]'s acquire, renew and
        /// release are all blind conditional writes, and its tests assert
        /// through those rather than by selecting. **A `find`, a `one()` or a
        /// `RETURNING` added here would compile, pass on Postgres, and fail on
        /// the unit tier** — which is the awkward direction, since the unit
        /// tier is the one that runs on every `cargo test`. Whoever needs to
        /// read this row should read it as text and parse it, or give the
        /// column a `String` field.
        pub claimed_at: OffsetDateTime,
        /// When the claim lapses unless renewed. Also the database's clock,
        /// and [`Self::claimed_at`]'s decode warning applies here unchanged.
        pub expires_at: OffsetDateTime,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

use entity::{Column as ClaimColumn, Entity as ClaimEntity};

/// The two SQL dialects this gear ships, and the clock arithmetic each spells
/// differently.
///
/// Lifted from `coord`'s `Dialect` (`gears/bss/libs/coord/src/lease/
/// manager.rs`), including its structure: `MySQL` is rejected once, in
/// [`Dialect::of`], so the per-expression matches stay exhaustive over two
/// variants without a `panic!` this workspace's lints would refuse. This gear
/// refuses `MySQL` at the migration too
/// (`m20260907_000003_leader_claims::up_ddl`), so the arm is unreachable in
/// practice and is an error rather than an assumption.
#[derive(Clone, Copy, Debug)]
enum Dialect {
    Postgres,
    Sqlite,
}

impl Dialect {
    fn of(engine: &str) -> anyhow::Result<Self> {
        match engine {
            "postgres" => Ok(Self::Postgres),
            "sqlite" => Ok(Self::Sqlite),
            other => Err(anyhow::anyhow!(
                "qa-insights' leader claim supports postgres and sqlite only, not {other}"
            )),
        }
    }

    /// "Now", on the database's clock.
    fn now(self) -> SimpleExpr {
        match self {
            Self::Postgres => Expr::cust("NOW()"),
            Self::Sqlite => Expr::cust("datetime('now')"),
        }
    }

    /// "Now + `ttl`", on the database's clock — the value a claim or a renewal
    /// pushes `expires_at` to.
    fn expiry(self, ttl: Duration) -> SimpleExpr {
        let seconds = ttl.as_secs();
        match self {
            Self::Postgres => Expr::cust(format!("NOW() + INTERVAL '{seconds} seconds'")),
            Self::Sqlite => Expr::cust(format!("datetime('now', '+{seconds} seconds')")),
        }
    }

    /// "This claim has lapsed", on the database's clock.
    ///
    /// `SQLite` compares through `datetime()` on both sides rather than as raw
    /// text: the epoch sentinel the `INSERT` writes arrives as `SeaORM`'s
    /// RFC-3339 (`1970-01-01T00:00:00Z`) while every later write comes from
    /// `datetime('now')` (space-separated, no zone), and a lexicographic `<=`
    /// over two spellings of the same instant is wrong in whichever direction
    /// happens to hurt. `coord`'s dialect carries the same normalisation and
    /// the same warning. Postgres compares native `TIMESTAMPTZ`.
    fn expired(self) -> SimpleExpr {
        match self {
            Self::Postgres => Expr::col(ClaimColumn::ExpiresAt).lte(self.now()),
            Self::Sqlite => Expr::cust("datetime(expires_at) <= datetime('now')"),
        }
    }

    /// "This claim is still live", the negation [`Self::expired`] renews
    /// against.
    fn live(self) -> SimpleExpr {
        match self {
            Self::Postgres => Expr::col(ClaimColumn::ExpiresAt).gt(self.now()),
            Self::Sqlite => Expr::cust("datetime(expires_at) > datetime('now')"),
        }
    }
}

/// Runs a role's work only while this replica holds that role's claim row.
///
/// Bound to [`ROLE_JIRA_POLLER`](super::ROLE_JIRA_POLLER) alone in
/// `crate::gear`; the other two roles keep
/// [`NoopLeaderElector`](super::NoopLeaderElector). See this module's header
/// for the CAS and [`super`]'s for why only one role needs it.
pub struct ClaimRowElector {
    db: Arc<DbProvider>,
    /// This replica's identity, fresh per process. A restart is a new holder,
    /// which is what stops a restarted process from renewing the claim its
    /// previous life left behind.
    holder: Uuid,
    ttl: Duration,
    renew_every: Duration,
    retry_every: Duration,
}

/// Hand-written because `DbProvider` has no `Debug`, and because the field
/// worth printing is the holder id: it is what an operator matches against the
/// `holder` column when asking which replica is polling.
impl std::fmt::Debug for ClaimRowElector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimRowElector")
            .field("holder", &self.holder)
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl ClaimRowElector {
    /// Build one with the shipped cadences.
    #[must_use]
    pub fn new(db: Arc<DbProvider>) -> Self {
        Self::with_timings(db, DEFAULT_TTL, DEFAULT_RENEW_EVERY, DEFAULT_RETRY_EVERY)
    }

    /// [`Self::new`] with the three durations as parameters.
    ///
    /// The tests are the only other caller: a suite that had to wait a real
    /// [`DEFAULT_RETRY_EVERY`] to observe a takeover would be a suite nobody
    /// runs. Not `#[cfg(test)]`, so the constructor a test drives is the one
    /// production compiles.
    #[must_use]
    pub fn with_timings(
        db: Arc<DbProvider>,
        ttl: Duration,
        renew_every: Duration,
        retry_every: Duration,
    ) -> Self {
        Self {
            db,
            holder: Uuid::new_v4(),
            ttl,
            renew_every,
            retry_every,
        }
    }

    /// This replica's holder id, as it appears in the `holder` column.
    #[must_use]
    pub fn holder(&self) -> Uuid {
        self.holder
    }

    /// Take `role`'s claim if it is free, expired, or already ours.
    ///
    /// `Ok(false)` is the ordinary answer for a replica that is not the
    /// leader, not a failure: another holder has a live claim. `Err` is a
    /// database failure, which the caller logs and retries rather than
    /// treating as either answer — silently reading it as "not ours" would
    /// idle the role for as long as the database is unhappy, and reading it as
    /// "ours" would be the double-launch this whole module exists to stop.
    ///
    /// # Errors
    ///
    /// The connection, an unsupported dialect, or either statement failing.
    async fn acquire(&self, role: &str) -> anyhow::Result<bool> {
        let conn = self.db.conn()?;
        // `Db::db_engine`, not the connection's: `DbConn` deliberately exposes
        // no `SeaORM` handle at all, so this is the one place the dialect is
        // nameable. The call clones an `Arc` and nothing else.
        let dialect = Dialect::of(self.db.db().db_engine())?;
        let scope = AccessScope::for_tenant(CLAIM_TENANT);

        // Step 1: take over a row that has lapsed, or re-take our own. See the
        // module header for why this alone serialises two acquirers.
        let taken = ClaimEntity::update_many()
            .col_expr(ClaimColumn::Holder, Expr::value(self.holder))
            .col_expr(ClaimColumn::ClaimedAt, dialect.now())
            .col_expr(ClaimColumn::ExpiresAt, dialect.expiry(self.ttl))
            .filter(
                Condition::all()
                    .add(ClaimColumn::TenantId.eq(CLAIM_TENANT))
                    .add(ClaimColumn::Role.eq(role))
                    .add(
                        Condition::any()
                            .add(dialect.expired())
                            // Ours already: a term that ended without the
                            // release landing would otherwise lock this
                            // replica out of its own role for a whole TTL.
                            // Only the true holder can match this.
                            .add(ClaimColumn::Holder.eq(self.holder)),
                    ),
            )
            .secure()
            .scope_with(&scope)
            .exec(&conn)
            .await?;
        if taken.rows_affected > 0 {
            return Ok(true);
        }

        // Step 2: there is no row for this role yet (or there is one and it is
        // live and someone else's, in which case the unique index says so).
        // The epoch sentinel carries no worker clock; the claim itself is the
        // step-1 update below, on the database's.
        let row = entity::ActiveModel {
            id: ActiveValue::Set(Uuid::new_v4()),
            tenant_id: ActiveValue::Set(CLAIM_TENANT),
            role: ActiveValue::Set(role.to_owned()),
            holder: ActiveValue::Set(self.holder),
            claimed_at: ActiveValue::Set(OffsetDateTime::UNIX_EPOCH),
            expires_at: ActiveValue::Set(OffsetDateTime::UNIX_EPOCH),
        };
        match toolkit_db::secure::secure_insert::<ClaimEntity>(row, &scope, &conn).await {
            Ok(_) => {}
            // The peer that beat us to the first insert holds a live claim.
            Err(ScopeError::Db(sea_orm::DbErr::RecordNotInserted)) => return Ok(false),
            Err(e) if e.is_unique_violation() => return Ok(false),
            Err(e) => return Err(e.into()),
        }

        // Claim the sentinel we just inserted, on the database's clock. Within
        // this replica's own view it always matches — the epoch is expired —
        // but a peer can have stolen it in the microseconds since, which is
        // exactly what the zero-rows answer means.
        let claimed = ClaimEntity::update_many()
            .col_expr(ClaimColumn::Holder, Expr::value(self.holder))
            .col_expr(ClaimColumn::ClaimedAt, dialect.now())
            .col_expr(ClaimColumn::ExpiresAt, dialect.expiry(self.ttl))
            .filter(
                Condition::all()
                    .add(ClaimColumn::TenantId.eq(CLAIM_TENANT))
                    .add(ClaimColumn::Role.eq(role))
                    .add(ClaimColumn::Holder.eq(self.holder))
                    .add(dialect.expired()),
            )
            .secure()
            .scope_with(&scope)
            .exec(&conn)
            .await?;
        Ok(claimed.rows_affected > 0)
    }

    /// Push `role`'s expiry forward, if the claim is still live and still
    /// ours.
    ///
    /// `Ok(false)` means it is not — the claim lapsed and someone else took
    /// it, which the caller turns into a cancelled term.
    ///
    /// # Errors
    ///
    /// The connection, an unsupported dialect, or the statement failing.
    async fn renew(&self, role: &str) -> anyhow::Result<bool> {
        let conn = self.db.conn()?;
        // `Db::db_engine`, not the connection's: `DbConn` deliberately exposes
        // no `SeaORM` handle at all, so this is the one place the dialect is
        // nameable. The call clones an `Arc` and nothing else.
        let dialect = Dialect::of(self.db.db().db_engine())?;
        let scope = AccessScope::for_tenant(CLAIM_TENANT);

        let renewed = ClaimEntity::update_many()
            .col_expr(ClaimColumn::ExpiresAt, dialect.expiry(self.ttl))
            .filter(
                Condition::all()
                    .add(ClaimColumn::TenantId.eq(CLAIM_TENANT))
                    .add(ClaimColumn::Role.eq(role))
                    .add(ClaimColumn::Holder.eq(self.holder))
                    // Live, not merely ours: a lapsed row still carrying our
                    // holder id has already been abandoned, and renewing it
                    // would resurrect a claim another replica may be about to
                    // take.
                    .add(dialect.live()),
            )
            .secure()
            .scope_with(&scope)
            .exec(&conn)
            .await?;
        Ok(renewed.rows_affected > 0)
    }

    /// Give `role` up, so the next contender does not have to wait out the
    /// TTL.
    ///
    /// Best-effort and deliberately infallible to the caller: the claim
    /// expires on its own, so a failed release costs one TTL of idleness and
    /// nothing else. It is guarded on our own holder id, so a release can
    /// never delete the claim of whoever took over from us.
    async fn release(&self, role: &str) {
        let released = async {
            let conn = self.db.conn()?;
            let scope = AccessScope::for_tenant(CLAIM_TENANT);
            ClaimEntity::delete_many()
                .filter(
                    Condition::all()
                        .add(ClaimColumn::TenantId.eq(CLAIM_TENANT))
                        .add(ClaimColumn::Role.eq(role))
                        .add(ClaimColumn::Holder.eq(self.holder)),
                )
                .secure()
                .scope_with(&scope)
                .exec(&conn)
                .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        if let Err(error) = released {
            warn!(
                role,
                holder = %self.holder,
                %error,
                "failed to release the leader claim; it will lapse on its own after the TTL",
            );
        }
    }

    /// Run `work` for one leadership term, renewing the claim underneath it.
    ///
    /// The work gets a **child** of the caller's token, so it stops on gear
    /// shutdown and on a lost claim through the same path. Returns whatever
    /// the work returned; the caller releases the claim either way.
    async fn hold_and_run(
        &self,
        role: &str,
        cancel: &CancellationToken,
        work: &LeaderWorkFn,
    ) -> anyhow::Result<()> {
        let term = cancel.child_token();
        let mut running = std::pin::pin!(work(term.clone()));
        let mut heartbeat = tokio::time::interval_at(
            tokio::time::Instant::now() + self.renew_every,
            self.renew_every,
        );
        // When the claim was last provably ours. A renewal that *fails* leaves
        // us unsure rather than dispossessed, and this is what bounds how long
        // we go on acting as leader while unsure: past the TTL, the row we
        // hold has lapsed by definition and another replica may already have
        // it.
        let mut last_confirmed = tokio::time::Instant::now();

        loop {
            tokio::select! {
                result = &mut running => return result,
                // Once the term is cancelled there is nothing left to renew:
                // the arm switches off and this loop is only waiting for the
                // work to notice its token and return.
                _ = heartbeat.tick(), if !term.is_cancelled() => {
                    last_confirmed = self.heartbeat(role, &term, last_confirmed).await;
                }
            }
        }
    }

    /// One heartbeat: renew the claim, and decide whether the term survives
    /// it. Answers the instant the claim was last provably ours.
    ///
    /// A lost claim ends the term outright. A renewal that *failed* is weaker
    /// evidence — the database may simply be briefly unreachable — so
    /// [`Self::after_a_failed_renewal`] ends it only once a whole TTL has
    /// passed without a confirmation, by which point the row we think we hold
    /// has lapsed and another replica may already have taken it.
    async fn heartbeat(
        &self,
        role: &str,
        term: &CancellationToken,
        last_confirmed: tokio::time::Instant,
    ) -> tokio::time::Instant {
        match self.renew(role).await {
            Ok(true) => return tokio::time::Instant::now(),
            Ok(false) => self.end_term(role, term, "the leader claim was lost"),
            Err(error) => self.after_a_failed_renewal(role, term, last_confirmed, &error),
        }
        last_confirmed
    }

    /// Stop the current term, saying why.
    ///
    /// The work is cancelled rather than abandoned: it holds a child of the
    /// caller's token, so this is the same shutdown the gear fires and the
    /// ticker stops the way it already knows how to.
    fn end_term(&self, role: &str, term: &CancellationToken, why: &str) {
        warn!(role, holder = %self.holder, why, "stopping this term's work");
        term.cancel();
    }

    /// What a failed renewal means, which is "we do not know" rather than "we
    /// lost".
    ///
    /// Logged every time and acted on only past the TTL: a claim that has not
    /// been confirmed for that long has lapsed by definition, whatever the
    /// reason, and continuing to poll on it is the double-launch this module
    /// exists to prevent.
    fn after_a_failed_renewal(
        &self,
        role: &str,
        term: &CancellationToken,
        last_confirmed: tokio::time::Instant,
        error: &anyhow::Error,
    ) {
        warn!(role, holder = %self.holder, %error, "failed to renew the leader claim");
        if last_confirmed.elapsed() >= self.ttl {
            self.end_term(
                role,
                term,
                "the leader claim has not been confirmed for a whole TTL",
            );
        }
    }
}

#[async_trait]
impl LeaderElector for ClaimRowElector {
    /// Contend for `role` until `cancel` fires, running `work` for each term
    /// this replica wins.
    ///
    /// **It loops.** A replica that loses now must be able to take over when
    /// the holder dies, which is the entire reason this exists rather than a
    /// chart assertion; returning on the first loss would make every replica
    /// but one permanently inert. So a loser waits `retry_every` and contends
    /// again, and a term that ends without the gear shutting down is followed
    /// by another attempt.
    ///
    /// The only ways out are `cancel` — the gear shutting down — and an error
    /// from the work itself, which `crate::gear`'s tickers log and treat as a
    /// ticker that has exited. A database failure while contending is *not*
    /// one of them: it is logged and retried, because the alternative is a
    /// deployment that stops polling JIRA because the database was briefly
    /// unreachable.
    async fn run_role(
        &self,
        role: &str,
        cancel: CancellationToken,
        work: LeaderWorkFn,
    ) -> anyhow::Result<()> {
        loop {
            if cancel.is_cancelled() {
                return Ok(());
            }

            match self.acquire(role).await {
                Ok(true) => {
                    info!(role, holder = %self.holder, "took the leader claim");
                    let outcome = self.hold_and_run(role, &cancel, &work).await;
                    // Released **before** the shutdown check below, and that
                    // order is deliberate: a rolling restart would otherwise
                    // leave the role unheld for a whole TTL while the departing
                    // replica returns. The consequence is that between this
                    // line and the check, the claim is free — so a peer whose
                    // retry lands in that window wins a term of its own. In
                    // production that is the correct answer (the role *is*
                    // free, and this replica is on its way out). In a test
                    // where the shutdown is what ends the winner's term, it
                    // means the loser can run a second pass; see
                    // `domain::service::jira_poller_tests::
                    // two_concurrent_pollers_produce_one_rerun`'s own note on
                    // what still holds when it does.
                    self.release(role).await;
                    outcome?;
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    info!(
                        role,
                        holder = %self.holder,
                        "the leadership term ended without a shutdown; contending again",
                    );
                }
                Ok(false) => {}
                Err(error) => warn!(
                    role,
                    holder = %self.holder,
                    %error,
                    "failed to contend for the leader claim; retrying",
                ),
            }

            tokio::select! {
                // Biased so that a token cancelled while we slept is observed
                // as a shutdown rather than as a chance to contend once more.
                biased;
                () = cancel.cancelled() => return Ok(()),
                () = tokio::time::sleep(self.retry_every) => {}
            }
        }
    }
}

/// Elector tests, on the unit tier's in-memory `SQLite`.
///
/// **What this tier can and cannot falsify.** Both electors here share one
/// `SQLite` database, so the claim table is genuinely shared and every
/// assertion below about *who wins* is real. What `SQLite` cannot do is let
/// two writers actually overlap inside the database — it serialises them — so
/// it cannot falsify the row-locking half of the header's argument. That is
/// what `domain::service::jira_poller_tests::
/// two_concurrent_pollers_produce_one_rerun` is for, behind `--features
/// integration` on real Postgres.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
    use tokio_util::sync::CancellationToken;
    use toolkit_db::secure::SecureUpdateExt;
    use toolkit_security::AccessScope;

    use super::{
        CLAIM_TENANT, ClaimColumn, ClaimEntity, ClaimRowElector, Dialect, LeaderElector,
        LeaderWorkFn,
    };
    use crate::domain::service::DbProvider;
    use crate::infra::leader::{ROLE_JIRA_POLLER, work_fn};
    use crate::infra::storage::test_db::inmem_db;

    /// One shared in-memory database, over which any number of electors are
    /// built — the shape the property under test needs, since an elector with
    /// its own database wins every time and proves nothing.
    async fn shared_db() -> Arc<DbProvider> {
        Arc::new(DbProvider::new(inmem_db().await))
    }

    /// An elector with test cadences: a two-second TTL and a 10ms retry, so a
    /// takeover is observable inside a test rather than inside a minute.
    fn elector(db: &Arc<DbProvider>) -> ClaimRowElector {
        ClaimRowElector::with_timings(
            Arc::clone(db),
            Duration::from_secs(2),
            Duration::from_millis(500),
            Duration::from_millis(10),
        )
    }

    /// Age `role`'s claim out, the way a crashed holder's does: the row keeps
    /// its holder id and `expires_at` moves into the past.
    ///
    /// The one statement in this file that is not one of the elector's own,
    /// and it goes through the same dialect arithmetic production writes with
    /// — a fixture that stamped a Rust instant here would be testing a row
    /// shape the elector never produces.
    async fn expire_the_claim(db: &Arc<DbProvider>, role: &str) {
        let conn = db.conn().unwrap();
        let dialect = Dialect::of(db.db().db_engine()).unwrap();
        ClaimEntity::update_many()
            .col_expr(ClaimColumn::ExpiresAt, dialect.expiry(Duration::ZERO))
            .filter(
                Condition::all()
                    .add(ClaimColumn::TenantId.eq(CLAIM_TENANT))
                    .add(ClaimColumn::Role.eq(role)),
            )
            .secure()
            .scope_with(&AccessScope::for_tenant(CLAIM_TENANT))
            .exec(&conn)
            .await
            .unwrap();
    }

    // Every assertion below reads the claim through the elector's own answers
    // -- `acquire` and `renew` -- rather than by selecting the row. That is
    // not squeamishness about a fixture: production never decodes this row
    // either (see the module header -- every statement is a blind conditional
    // write), so a test that decoded it would be exercising a path nothing
    // ships, and on the `SQLite` tier it would have to decode a
    // `datetime('now')` string that no Rust type here claims to parse.

    /// **The first acquirer wins and the second loses.**
    ///
    /// The property the whole module exists for, at the smallest scale that
    /// can hold it: one database, two electors, one role. The third assertion
    /// is what makes the second non-vacuous — the loser did not merely fail
    /// to write, the winner really holds a renewable claim.
    #[tokio::test]
    async fn one_of_two_electors_holds_the_claim() {
        let db = shared_db().await;
        let a = elector(&db);
        let b = elector(&db);

        assert!(
            a.acquire(ROLE_JIRA_POLLER).await.unwrap(),
            "the first acquirer wins"
        );
        assert!(
            !b.acquire(ROLE_JIRA_POLLER).await.unwrap(),
            "the second must lose while the first's claim is live; a second winner is \
             the double-launch this table exists to prevent"
        );
        assert!(a.renew(ROLE_JIRA_POLLER).await.unwrap());
        assert!(!b.renew(ROLE_JIRA_POLLER).await.unwrap());
    }

    /// **A crashed holder does not keep the role.**
    ///
    /// The claim is aged out rather than waited out, but it is the same row
    /// state a replica that stopped renewing leaves behind: the holder id is
    /// still there and `expires_at` has passed.
    #[tokio::test]
    async fn an_expired_claim_is_taken_over() {
        let db = shared_db().await;
        let dead = elector(&db);
        let live = elector(&db);

        assert!(dead.acquire(ROLE_JIRA_POLLER).await.unwrap());
        assert!(
            !live.acquire(ROLE_JIRA_POLLER).await.unwrap(),
            "not while the first claim is live"
        );

        expire_the_claim(&db, ROLE_JIRA_POLLER).await;

        assert!(
            live.acquire(ROLE_JIRA_POLLER).await.unwrap(),
            "an expired claim must be takeable, or a crashed replica retires the role \
             for good"
        );
        assert!(live.renew(ROLE_JIRA_POLLER).await.unwrap());
    }

    /// **The holder that lapsed cannot renew its way back in.**
    ///
    /// Two refusals, and they are different halves of `renew`'s filter. The
    /// first is the `live` half alone — the row still carries this holder's
    /// id and nobody has taken it, and without that half a replica returning
    /// from a long stall would quietly push its own expiry forward over a
    /// claim it had already abandoned. The second is the holder half, once
    /// someone else has it.
    #[tokio::test]
    async fn a_lapsed_holder_cannot_renew() {
        let db = shared_db().await;
        let stalled = elector(&db);
        let taker = elector(&db);

        assert!(stalled.acquire(ROLE_JIRA_POLLER).await.unwrap());
        expire_the_claim(&db, ROLE_JIRA_POLLER).await;

        assert!(
            !stalled.renew(ROLE_JIRA_POLLER).await.unwrap(),
            "a lapsed claim is abandoned, not renewable, even by the replica whose \
             holder id the row still carries"
        );

        assert!(taker.acquire(ROLE_JIRA_POLLER).await.unwrap());
        assert!(
            !stalled.renew(ROLE_JIRA_POLLER).await.unwrap(),
            "and once someone else holds it, the holder half of the filter refuses too"
        );
        assert!(
            taker.renew(ROLE_JIRA_POLLER).await.unwrap(),
            "the replica that actually holds it must be able to renew"
        );
    }

    /// **A released claim is free immediately**, so a rolling restart does not
    /// leave the role unheld for a TTL.
    #[tokio::test]
    async fn a_released_claim_is_takeable_at_once() {
        let db = shared_db().await;
        let first = elector(&db);
        let second = elector(&db);

        assert!(first.acquire(ROLE_JIRA_POLLER).await.unwrap());
        assert!(
            !second.acquire(ROLE_JIRA_POLLER).await.unwrap(),
            "non-vacuity: the claim really was held before the release"
        );

        first.release(ROLE_JIRA_POLLER).await;

        assert!(
            second.acquire(ROLE_JIRA_POLLER).await.unwrap(),
            "a released claim must be takeable without waiting out the TTL"
        );
    }

    /// **A release only ever deletes the releaser's own claim.**
    ///
    /// The failure this forbids is a slow replica releasing on its way out and
    /// dropping the claim its successor is holding, which would hand the role
    /// to a third — a release racing a takeover is exactly when that happens.
    #[tokio::test]
    async fn a_release_does_not_touch_someone_elses_claim() {
        let db = shared_db().await;
        let departed = elector(&db);
        let successor = elector(&db);

        assert!(departed.acquire(ROLE_JIRA_POLLER).await.unwrap());
        expire_the_claim(&db, ROLE_JIRA_POLLER).await;
        assert!(successor.acquire(ROLE_JIRA_POLLER).await.unwrap());

        departed.release(ROLE_JIRA_POLLER).await;

        assert!(
            successor.renew(ROLE_JIRA_POLLER).await.unwrap(),
            "the departing replica's release must not free its successor's claim"
        );
    }

    /// **The holder runs the work and the loser does not**, which is the
    /// property `run_role` adds on top of `acquire`.
    ///
    /// Both terms end when the work cancels the shared token — the same
    /// shutdown `crate::gear` fires — because `run_role` loops until then by
    /// design: see its own doc for why a loser returning early would make
    /// every replica but one permanently inert.
    #[tokio::test]
    async fn only_the_holder_runs_the_work() {
        let db = shared_db().await;
        let a = elector(&db);
        let b = elector(&db);
        let stop = CancellationToken::new();
        let ran = Arc::new(AtomicUsize::new(0));

        let work_for = |token: CancellationToken, counter: Arc<AtomicUsize>| {
            work_fn(move |_term| {
                let token = token.clone();
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    token.cancel();
                    Ok(())
                }
            })
        };

        let (first, second) = tokio::join!(
            a.run_role(
                ROLE_JIRA_POLLER,
                stop.clone(),
                work_for(stop.clone(), Arc::clone(&ran))
            ),
            b.run_role(
                ROLE_JIRA_POLLER,
                stop.clone(),
                work_for(stop.clone(), Arc::clone(&ran))
            ),
        );
        first.unwrap();
        second.unwrap();

        assert_eq!(
            ran.load(Ordering::SeqCst),
            1,
            "exactly one of the two electors may run the work"
        );
    }

    /// **A stolen claim stops the term's work**, which is the property that
    /// turns this module's stated residual into a bounded one.
    ///
    /// Everything else here tests `acquire`, `renew` and `release` — the
    /// statements. This is the only test that drives
    /// [`ClaimRowElector::hold_and_run`] and the heartbeat underneath it, and
    /// it is the behaviour the task actually changed: `run_role` loops *for*
    /// failover, so the moment a dispossessed holder stops noticing it has
    /// been dispossessed, two replicas poll side by side forever and nothing
    /// goes red. The module header's "one tenant's `poll_once`" bound is an
    /// argument without this test and a demonstrated property with it.
    ///
    /// # Why it is deterministic rather than timing-dependent
    ///
    /// The steal **retries in a loop** instead of sleeping past a TTL and
    /// hoping. The holder renews every 50ms against a 200ms claim, so a naive
    /// "expire it, then take it" would race that heartbeat and hang whenever
    /// the renewal landed in between; expiring and re-attempting until the
    /// takeover succeeds cannot lose that race, only repeat it. The outer
    /// timeout is the failure mode for a regression: a `hold_and_run` that
    /// never cancels its term would otherwise hang this suite instead of
    /// failing it.
    ///
    /// # What it does not cover
    ///
    /// [`ClaimRowElector::after_a_failed_renewal`](super::ClaimRowElector::after_a_failed_renewal)'s
    /// TTL bound — the arm that
    /// ends a term after a whole TTL of renewals *erroring* rather than
    /// answering `false`. Reaching it needs a database that fails on demand,
    /// which this tier has no seam for; it is argued in that method's own doc
    /// and unexecuted.
    #[tokio::test]
    async fn a_stolen_claim_stops_the_term() {
        let db = shared_db().await;
        // A short TTL and a heartbeat well inside it: the holder is healthy
        // and renewing right up until the claim is taken out from under it,
        // which is the interesting case. A holder that had simply stopped
        // renewing would reach the same `Ok(false)` by a less demanding route.
        let holder = ClaimRowElector::with_timings(
            Arc::clone(&db),
            Duration::from_millis(200),
            Duration::from_millis(50),
            Duration::from_millis(10),
        );
        let thief = elector(&db);

        assert!(
            holder.acquire(ROLE_JIRA_POLLER).await.unwrap(),
            "the holder must start out holding it"
        );

        // Work that does nothing but park on its term token, so the *only*
        // way `hold_and_run` returns is `end_term` cancelling it.
        let parked = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&parked);
        let work: LeaderWorkFn = work_fn(move |term| {
            let observed = Arc::clone(&observed);
            async move {
                term.cancelled().await;
                observed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });

        // Never fired: this test is about losing the claim, not about
        // shutting down, and the two must not be confusable.
        let never = CancellationToken::new();

        let steal = async {
            loop {
                expire_the_claim(&db, ROLE_JIRA_POLLER).await;
                if thief.acquire(ROLE_JIRA_POLLER).await.unwrap() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };

        let term = tokio::time::timeout(
            Duration::from_secs(10),
            async { tokio::join!(holder.hold_and_run(ROLE_JIRA_POLLER, &never, &work), steal).0 },
        )
        .await
        .expect(
            "hold_and_run must return once the claim is stolen; hanging here means the \
             heartbeat no longer ends the term, and a dispossessed replica would poll \
             alongside the new holder indefinitely",
        );

        term.unwrap();
        assert_eq!(
            parked.load(Ordering::SeqCst),
            1,
            "the term's work must have observed its own token being cancelled"
        );
        assert!(
            !never.is_cancelled(),
            "and the caller's token must not have been touched: losing a claim is not \
             a shutdown, and `run_role` has to be able to tell them apart to contend again"
        );
        assert!(
            !holder.renew(ROLE_JIRA_POLLER).await.unwrap(),
            "the dispossessed holder must not still be able to renew"
        );
        assert!(thief.renew(ROLE_JIRA_POLLER).await.unwrap());
    }

    /// **The takeover path, on the dialect this gear actually deploys.**
    ///
    /// [`an_expired_claim_is_taken_over`] proves it on `SQLite`, and the two
    /// dialects spell the clock arithmetic differently — `NOW() + INTERVAL`
    /// against `datetime('now', '+n seconds')`, a native `TIMESTAMPTZ` compare
    /// against a `datetime()`-normalised one. There are two ways the Postgres
    /// half can be wrong and they fail in opposite directions: a claim that
    /// never expires retires the role at the first crash, and one that always
    /// expires hands it to everyone. The second is what
    /// `domain::service::jira_poller_tests::two_concurrent_pollers_produce_one_rerun`
    /// would catch; this is the first, and nothing else covers it.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn an_expired_claim_is_taken_over_on_postgres() {
        let harness = crate::infra::storage::test_db::pg_db().await;
        let db = Arc::new(DbProvider::new(harness.db.clone()));
        let dead = elector(&db);
        let live = elector(&db);

        assert!(dead.acquire(ROLE_JIRA_POLLER).await.unwrap());
        assert!(
            !live.acquire(ROLE_JIRA_POLLER).await.unwrap(),
            "not while the first claim is live -- without this the test below would \
             pass against a Postgres predicate that expires everything"
        );

        expire_the_claim(&db, ROLE_JIRA_POLLER).await;

        assert!(
            live.acquire(ROLE_JIRA_POLLER).await.unwrap(),
            "an expired claim must be takeable on Postgres too, or a crashed replica \
             retires the role for good"
        );
        assert!(live.renew(ROLE_JIRA_POLLER).await.unwrap());
        assert!(!dead.renew(ROLE_JIRA_POLLER).await.unwrap());
    }

    /// An unsupported dialect is refused rather than silently mis-compared.
    ///
    /// The arm is unreachable in this gear — the migration refuses `MySQL`
    /// outright — and an unexecuted error path is the same defect class as an
    /// untested column name.
    #[test]
    fn an_unsupported_dialect_is_refused() {
        let err = Dialect::of("mysql").expect_err("mysql is refused");
        assert!(err.to_string().contains("postgres and sqlite"), "{err}");
    }
}
