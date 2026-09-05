pub mod executor;
/// Leader election for the dispatcher ticker.
pub mod leader;
/// Live log fan-out for the SSE endpoint. Task 15.
pub mod logs;
/// The product-plugin resolver, reached through the `ClientHub` — see the
/// module's own header for why it resolves per call.
pub mod product_plugin;
pub mod storage;

/// The generic `AppServices<R, Q, S>` container bound to the `SeaORM`
/// repositories - the type the REST handlers extract and the gear bootstrap
/// builds.
///
/// # Why it lives here and not in `gear.rs`
///
/// Both shipped sibling gears declare their equivalent in `gear.rs`, and the
/// plan's Step 5 says this one should too. It is here instead because the REST
/// handlers need it and they land a stage before `gear.rs` exists; putting it
/// in a not-yet-written module would have meant writing a stub `gear.rs` whose
/// only content was a type alias.
///
/// Infra is the right home on the merits as well: the alias is precisely *the
/// domain services bound to the concrete repositories*, and those repositories
/// are declared right here. The domain layer could not hold it - it would have
/// to name `OrmRunsRepository`, which is the dependency direction this crate's
/// layering forbids.
///
/// **The gear bootstrap must `use` this rather than declare its own.** Two
/// aliases for the same instantiation would compile and would silently allow
/// the handlers and the lifecycle tasks to be wired to different repository
/// types.
pub(crate) type ConcreteAppServices = crate::domain::service::AppServices<
    OrmRunsRepository,
    OrmQueueRepository,
    OrmSchedulesRepository,
>;

use storage::{OrmQueueRepository, OrmRunsRepository, OrmSchedulesRepository};
