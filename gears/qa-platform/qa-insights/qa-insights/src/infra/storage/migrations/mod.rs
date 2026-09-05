use sea_orm_migration::prelude::*;

mod m20260818_000001_initial;
mod m20260818_000002_offset_store;

pub struct Migrator;

/// Migrations are **append-only** and this list is in application order.
///
/// A new table is a new file, never an edit to an older one: an edit would
/// never be applied to a deployment that already ran the earlier version.
/// `down()` runs in the reverse of this order, which is why each migration's
/// own test module drives `MigrationTrait::down` on its own `Migration` rather
/// than looping over this list.
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260818_000001_initial::Migration),
            // `evbk_consumer_offsets` — superseded. It held a dependency's
            // durable progress for a transactional broker consumer that was
            // deleted along with the event-broker dependency it needed; the
            // table is inert and no code reads or writes it. Kept because it
            // has already run on deployed databases; see that file's header.
            Box::new(m20260818_000002_offset_store::Migration),
        ]
    }
}
