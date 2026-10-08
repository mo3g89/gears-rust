use sea_orm_migration::prelude::*;

mod m20260818_000001_initial;
mod m20260921_000002_smtp_credentials;
mod m20260929_000003_seed_run_completed_notification_claims;
mod m20260929_000004_run_completed_notification_cutoff;
mod m20260929_000005_drop_ingest_watermarks_last_swept_at;
mod m20260929_000006_ingest_watermarks_sweep_cursor;
mod m20260929_000007_opt_existing_tenants_into_success_notifications;
mod m20261007_000008_bare_credstore_refs;
mod m20261007_000009_run_projection_locks;

#[cfg(test)]
mod schema_behaviour_tests;

/// The chain as it stood **before** the two notification-backfill migrations,
/// `m20260929_000003_seed_run_completed_notification_claims` and
/// `m20260929_000004_run_completed_notification_cutoff`.
///
/// Exists for one test shape and it is the shape those migrations are *for*:
/// `infra::storage::test_db::inmem_db_before_the_notification_backfill` builds
/// a database at the previous revision, a test fills `qa_test_results` and
/// registers finished runs the way a running deployment would have, and then
/// `infra::storage::test_db::apply_pending_migrations` performs the upgrade.
/// A test that started from a fully-migrated empty database would run both
/// migrations over nothing and could not tell a working backfill from an
/// absent one.
///
/// Written out rather than derived by dropping the last *n* elements of
/// [`MigratorTrait::migrations`]: a later migration appended to that list would
/// silently move into "before", and this list naming exactly two entries is
/// what makes the next author notice.
#[cfg(test)]
pub fn migrations_before_the_notification_backfill() -> Vec<Box<dyn MigrationTrait>> {
    vec![
        Box::new(m20260818_000001_initial::Migration),
        Box::new(m20260921_000002_smtp_credentials::Migration),
    ]
}

pub struct Migrator;

/// This list is in application order, and from here on it is **append-only**.
///
/// A new table is a new file, never an edit to this one: an edit would never be
/// applied to a deployment that already ran the earlier version. `down()` runs
/// in the reverse of this order, which is why each migration's own test module
/// drives `MigrationTrait::down` on its own `Migration` rather than looping over
/// this list.
///
/// The chain was collapsed to a single migration before the platform's first
/// installation, when no deployment had run any of it: what were three
/// migrations declared the schema, added a table, and added a second table that
/// nothing ever read. Collapsing cost nothing then and cannot be repeated now.
///
/// `m20260921_000002_smtp_credentials` is the first migration added after that
/// collapse, and it is the proof of the append-only rule rather than an
/// exception to it: it adds two columns to `qa_notification_config` with
/// `ALTER TABLE`, because the initial migration has already run on the
/// deployments that need them.
///
/// `m20260929_000003_seed_run_completed_notification_claims` is the second,
/// and the first that changes no schema at all — it writes **rows**. It has to
/// run before `ReconcileService::reproject`'s new call to
/// `NotifyService::notify_run_completed` can reach a deployment's historical
/// runs; that file's header carries what it seeds and why the claim table is
/// the right place to say it.
///
/// `m20260929_000004_run_completed_notification_cutoff` is the third, and it
/// exists because the second is **not sufficient on its own**: the claim seed
/// can only name runs that left rows in `qa_test_results`, and 47% of the dev
/// stand's finished runs left none. Its header carries the measurement. The
/// two ship together and neither replaces the other.
///
/// `m20260929_000005_drop_ingest_watermarks_last_swept_at` is the fourth and
/// the first to remove anything: a column recording a sweep that was never
/// built, dropped on the owner's ruling.
///
/// `m20260929_000006_ingest_watermarks_sweep_cursor` is the fifth. It touches
/// the same table the fourth dropped a column from, but adds three new,
/// unrelated nullable columns: the reconcile sweep's within-window resume
/// point, which stops a lookback window wider than one pass' page budget from
/// being re-walked identically on every tick forever (second review, finding
/// #122). Its header carries why the cursor is not a second watermark and why
/// nothing compares it in SQL.
///
/// `m20260929_000007_opt_existing_tenants_into_success_notifications` is the
/// sixth, and the second that changes no schema — it writes one column of
/// existing **rows**. `notify_on_success` became a routing gate on the owner's
/// 2026-09-29 ruling, and it was inert before that, so on the one intermediate
/// build that notified with it inert an upgrade that left stored rows alone
/// would silence a tenant's passing runs. It brings those rows to the
/// behaviour that build had and leaves the column default at `FALSE`, so a
/// tenant onboarded afterwards is not opted in. Its header carries why the
/// default was not flipped instead, and why its re-execution guard is this
/// ledger rather than a `WHERE` clause.
///
/// `m20261007_000008_bare_credstore_refs` is the seventh, and the third that
/// changes no schema: it rewrites any stored `cred://name` in the three
/// credential-reference columns to `name`, because a reference now has
/// `credstore_sdk::SecretRef`'s one syntax with no scheme prefix and a kept
/// prefixed value would fail the JIRA `PUT`'s re-validation.
///
/// `m20261007_000009_run_projection_locks` is the eighth. It adds the per-run
/// lock row every writer of a run's results upserts first, so two
/// reprojections of one run can no longer both commit their batch, and it
/// deletes the older batches that race already stored.
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260818_000001_initial::Migration),
            Box::new(m20260921_000002_smtp_credentials::Migration),
            Box::new(m20260929_000003_seed_run_completed_notification_claims::Migration),
            Box::new(m20260929_000004_run_completed_notification_cutoff::Migration),
            Box::new(m20260929_000005_drop_ingest_watermarks_last_swept_at::Migration),
            Box::new(m20260929_000006_ingest_watermarks_sweep_cursor::Migration),
            Box::new(m20260929_000007_opt_existing_tenants_into_success_notifications::Migration),
            Box::new(m20261007_000008_bare_credstore_refs::Migration),
            Box::new(m20261007_000009_run_projection_locks::Migration),
        ]
    }
}
