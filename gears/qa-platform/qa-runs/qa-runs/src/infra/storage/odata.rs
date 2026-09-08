//! `OData` field allow-lists and column mappings for the paginated reads.
//!
//! # One enum, two consumers, and why that is the point
//!
//! Each `FilterField` enum here is passed to **both** the route
//! (`OperationBuilder::with_odata_filter::<F>()`, which is what publishes the
//! filterable fields in the `OpenAPI` document) and the repository
//! (`paginate_odata::<F, M, ..>`, which is what translates a filter into SQL).
//! Passing the same type to both is the only thing that stops the advertised
//! field set and the translatable field set drifting apart - a field advertised
//! but not mapped is a filter that fails at runtime, and a field mapped but not
//! advertised is a filter nobody knows exists.
//!
//! # The allow-lists are narrower than the tables
//!
//! Neither enum exposes every column. A field here is a promise: it is
//! filterable, sortable, and usable as a pagination cursor, which means the
//! query planner has to cope with it on a table of arbitrary size. The fields
//! chosen are the ones an operator actually filters a run history by, plus the
//! two - `created_at`/`enqueued_at` and `id` - that carry the cursor.
//!
//! `error` is deliberately absent from both. It is free text, it is sometimes
//! `OPAQUE_ERROR_TEXT` and sometimes an executor's message, and making it
//! filterable would invite `contains(error, ...)` scans over the whole history.

use sea_orm::Value;
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};
use toolkit_odata::filter::{FieldKind, FilterField};

use crate::infra::storage::entity::run::{
    Column as RunColumn, Entity as RunEntity, Model as RunModel,
};
use crate::infra::storage::entity::run_queue::{
    Column as QueueColumn, Entity as QueueEntity, Model as QueueModel,
};

/// Filterable, sortable and cursor-capable fields of the runs collection.
///
/// The tiebreaker pair is `created_at` descending plus `id`; see
/// `RunsRepository::list_page` for why the sort is newest-first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunFilterField {
    Id,
    Name,
    /// `sdk::RunState::as_str`'s spelling, because that is what the column
    /// stores. A filter of `state eq 'succeeded'` therefore works and
    /// `state eq 'Succeeded'` does not - the values are the persisted ones, not
    /// the Rust variant names.
    State,
    RunKind,
    /// Renamed from `PlatformId` (ruling G-3): the `OData` `$filter`/
    /// `$orderby` field name is now `environment_id`, matching both the REST
    /// DTO field of the same concept
    /// ([`RunDto::environment_id`](crate::api::rest::dto::RunDto::environment_id))
    /// and the `RunColumn::EnvironmentId` variant
    /// [`RunODataMapper::map_field`] below maps it to — variant, wire name and
    /// `Column` **variant** name all agree. The **physical column** is still
    /// `platform_id` (`entity/run.rs:39`'s `#[sea_orm(column_name =
    /// "platform_id")]`) — ruling B3, unmoved by this rename.
    EnvironmentId,
    Source,
    ScheduleId,
    ResolvedExclusive,
    IsValidation,
    CreatedAt,
    StartedAt,
    FinishedAt,
}

impl FilterField for RunFilterField {
    const FIELDS: &'static [Self] = &[
        Self::Id,
        Self::Name,
        Self::State,
        Self::RunKind,
        Self::EnvironmentId,
        Self::Source,
        Self::ScheduleId,
        Self::ResolvedExclusive,
        Self::IsValidation,
        Self::CreatedAt,
        Self::StartedAt,
        Self::FinishedAt,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Name => "name",
            Self::State => "state",
            Self::RunKind => "run_kind",
            Self::EnvironmentId => "environment_id",
            Self::Source => "source",
            Self::ScheduleId => "schedule_id",
            Self::ResolvedExclusive => "resolved_exclusive",
            Self::IsValidation => "is_validation",
            Self::CreatedAt => "created_at",
            Self::StartedAt => "started_at",
            Self::FinishedAt => "finished_at",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Id | Self::EnvironmentId | Self::ScheduleId => FieldKind::Uuid,
            Self::Name | Self::State | Self::RunKind | Self::Source => FieldKind::String,
            Self::ResolvedExclusive | Self::IsValidation => FieldKind::Bool,
            Self::CreatedAt | Self::StartedAt | Self::FinishedAt => FieldKind::DateTimeUtc,
        }
    }
}

/// Column mapping for [`RunFilterField`].
pub struct RunODataMapper;

impl FieldToColumn<RunFilterField> for RunODataMapper {
    type Column = RunColumn;

    fn map_field(field: RunFilterField) -> RunColumn {
        match field {
            RunFilterField::Id => RunColumn::Id,
            RunFilterField::Name => RunColumn::Name,
            RunFilterField::State => RunColumn::State,
            RunFilterField::RunKind => RunColumn::RunKind,
            RunFilterField::EnvironmentId => RunColumn::EnvironmentId,
            RunFilterField::Source => RunColumn::Source,
            RunFilterField::ScheduleId => RunColumn::ScheduleId,
            RunFilterField::ResolvedExclusive => RunColumn::ResolvedExclusive,
            RunFilterField::IsValidation => RunColumn::IsValidation,
            RunFilterField::CreatedAt => RunColumn::CreatedAt,
            RunFilterField::StartedAt => RunColumn::StartedAt,
            RunFilterField::FinishedAt => RunColumn::FinishedAt,
        }
    }
}

impl ODataFieldMapping<RunFilterField> for RunODataMapper {
    type Entity = RunEntity;

    fn extract_cursor_value(model: &RunModel, field: RunFilterField) -> Value {
        match field {
            RunFilterField::Id => Value::Uuid(Some(model.id)),
            RunFilterField::Name => Value::String(Some(model.name.clone())),
            RunFilterField::State => Value::String(Some(model.state.clone())),
            RunFilterField::RunKind => Value::String(Some(model.run_kind.clone())),
            RunFilterField::EnvironmentId => Value::Uuid(model.environment_id),
            RunFilterField::Source => Value::String(Some(model.source.clone())),
            RunFilterField::ScheduleId => Value::Uuid(model.schedule_id),
            RunFilterField::ResolvedExclusive => Value::Bool(Some(model.resolved_exclusive)),
            RunFilterField::IsValidation => Value::Bool(Some(model.is_validation)),
            RunFilterField::CreatedAt => {
                Value::TimeDateTimeWithTimeZone(Some(model.created_at))
            }
            RunFilterField::StartedAt => {
                Value::TimeDateTimeWithTimeZone(model.started_at)
            }
            RunFilterField::FinishedAt => {
                Value::TimeDateTimeWithTimeZone(model.finished_at)
            }
        }
    }
}

/// Filterable, sortable and cursor-capable fields of the run queue.
///
/// `environment_id` is here as well as being a first-class query parameter on
/// the endpoint — the same name, on purpose (ruling G-3), but not the same
/// mechanism: this is not redundancy. The guide's own remedy for a distorted
/// `queue_position` is *the environment-filtered call*, so the endpoint keeps a
/// plain parameter that a caller cannot get wrong (no `OData` syntax to
/// learn), while the `OData` field serves the general case (composed with
/// other filters, used as a sort or cursor key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueueFilterField {
    Id,
    RunId,
    /// Renamed from `PlatformId` (ruling G-3), matching
    /// [`RunFilterField::EnvironmentId`] and `QueueColumn::EnvironmentId`
    /// ([`QueueODataMapper::map_field`] below) — variant, wire name and
    /// `Column` **variant** name all agree. The **physical column** is still
    /// `platform_id` (`entity/run_queue.rs:29`'s `#[sea_orm(column_name =
    /// "platform_id")]`) — ruling B3, unmoved by this rename.
    EnvironmentId,
    /// One of the seven frozen queue-state names, as persisted - note
    /// `cancelled` with two `l`s, which is not the run state's spelling.
    State,
    RunKind,
    Source,
    Exclusive,
    EnqueuedAt,
    DispatchedAt,
    FinishedAt,
}

impl FilterField for QueueFilterField {
    const FIELDS: &'static [Self] = &[
        Self::Id,
        Self::RunId,
        Self::EnvironmentId,
        Self::State,
        Self::RunKind,
        Self::Source,
        Self::Exclusive,
        Self::EnqueuedAt,
        Self::DispatchedAt,
        Self::FinishedAt,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::RunId => "run_id",
            Self::EnvironmentId => "environment_id",
            Self::State => "state",
            Self::RunKind => "run_kind",
            Self::Source => "source",
            Self::Exclusive => "exclusive",
            Self::EnqueuedAt => "enqueued_at",
            Self::DispatchedAt => "dispatched_at",
            Self::FinishedAt => "finished_at",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Id | Self::RunId | Self::EnvironmentId => FieldKind::Uuid,
            Self::State | Self::RunKind | Self::Source => FieldKind::String,
            Self::Exclusive => FieldKind::Bool,
            Self::EnqueuedAt | Self::DispatchedAt | Self::FinishedAt => FieldKind::DateTimeUtc,
        }
    }
}

/// Column mapping for [`QueueFilterField`].
pub struct QueueODataMapper;

impl FieldToColumn<QueueFilterField> for QueueODataMapper {
    type Column = QueueColumn;

    fn map_field(field: QueueFilterField) -> QueueColumn {
        match field {
            QueueFilterField::Id => QueueColumn::Id,
            QueueFilterField::RunId => QueueColumn::RunId,
            QueueFilterField::EnvironmentId => QueueColumn::EnvironmentId,
            QueueFilterField::State => QueueColumn::State,
            QueueFilterField::RunKind => QueueColumn::RunKind,
            QueueFilterField::Source => QueueColumn::Source,
            QueueFilterField::Exclusive => QueueColumn::Exclusive,
            QueueFilterField::EnqueuedAt => QueueColumn::EnqueuedAt,
            QueueFilterField::DispatchedAt => QueueColumn::DispatchedAt,
            QueueFilterField::FinishedAt => QueueColumn::FinishedAt,
        }
    }
}

impl ODataFieldMapping<QueueFilterField> for QueueODataMapper {
    type Entity = QueueEntity;

    fn extract_cursor_value(model: &QueueModel, field: QueueFilterField) -> Value {
        match field {
            QueueFilterField::Id => Value::Uuid(Some(model.id)),
            QueueFilterField::RunId => Value::Uuid(Some(model.run_id)),
            QueueFilterField::EnvironmentId => Value::Uuid(Some(model.environment_id)),
            QueueFilterField::State => Value::String(Some(model.state.clone())),
            QueueFilterField::RunKind => Value::String(Some(model.run_kind.clone())),
            QueueFilterField::Source => Value::String(Some(model.source.clone())),
            QueueFilterField::Exclusive => Value::Bool(Some(model.exclusive)),
            QueueFilterField::EnqueuedAt => {
                Value::TimeDateTimeWithTimeZone(Some(model.enqueued_at))
            }
            QueueFilterField::DispatchedAt => {
                Value::TimeDateTimeWithTimeZone(model.dispatched_at)
            }
            QueueFilterField::FinishedAt => {
                Value::TimeDateTimeWithTimeZone(model.finished_at)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{QueueFilterField, RunFilterField, RunODataMapper};
    use toolkit_db::odata::sea_orm_filter::FieldToColumn;
    use toolkit_odata::filter::FilterField;

    /// **`FIELDS` must list every variant**, because it is what the route
    /// advertises *and* what the repository translates - a variant missing from
    /// it is a field that exists in the type and nowhere else, silently
    /// unfilterable. The module header names this exact hazard.
    ///
    /// The check is a count against the variants, because `FIELDS` is a
    /// hand-written slice and nothing else can see a gap in it: `map_field` is
    /// a total exhaustive match, so a variant added to *both* is already a
    /// compile error, and a variant missing from `FIELDS` is simply never
    /// iterated.
    ///
    /// **Corrected 2026-08-15.** The loop that shipped here iterated `FIELDS`
    /// and called `map_field` on each, with a doc claiming it "really pins the
    /// reverse direction - that `FIELDS` and the enum agree" and would "panic
    /// if a variant is ever added to `FIELDS` without a mapping arm". Neither
    /// is reachable, and deleting `Self::FinishedAt` from `FIELDS` left the
    /// whole suite green. A count is crude; it is also the only thing that
    /// fails on that mutation.
    #[test]
    fn every_run_field_variant_is_advertised() {
        let mut names: Vec<&str> = RunFilterField::FIELDS
            .iter()
            .map(FilterField::name)
            .collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate field name: {names:?}");
        assert_eq!(
            names.len(),
            RUN_FILTER_FIELD_VARIANTS,
            "a RunFilterField variant is missing from FIELDS, so it is \
             advertised nowhere and filterable by nobody: {names:?}"
        );

        // Every advertised field maps to a column. Total either way - the match
        // is exhaustive - so this pins nothing on its own; it is here so a
        // reader checking the pairing sees both halves in one place.
        for field in RunFilterField::FIELDS {
            let _column = RunODataMapper::map_field(*field);
        }
    }

    /// See [`every_run_field_variant_is_advertised`]. Paired with the
    /// exhaustive `name`/`kind` matches on the enum, which is what fails to
    /// compile when a variant is added.
    const RUN_FILTER_FIELD_VARIANTS: usize = 12;
    /// See [`RUN_FILTER_FIELD_VARIANTS`].
    const QUEUE_FILTER_FIELD_VARIANTS: usize = 10;

    #[test]
    fn every_queue_field_variant_is_advertised() {
        let mut names: Vec<&str> = QueueFilterField::FIELDS
            .iter()
            .map(FilterField::name)
            .collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate field name: {names:?}");
        assert_eq!(
            names.len(),
            QUEUE_FILTER_FIELD_VARIANTS,
            "a QueueFilterField variant is missing from FIELDS: {names:?}"
        );
    }

    /// The names are the **persisted** column names, which is what a caller
    /// writes in a `$filter`. Pinned for the two that are easy to get wrong:
    /// the run's decision field is `resolved_exclusive` (not `exclusive`, which
    /// is the queue row's), and the queue's clock is `enqueued_at` (not
    /// `created_at`).
    #[test]
    fn the_two_easily_confused_field_names_are_the_persisted_ones() {
        assert!(
            RunFilterField::FIELDS
                .iter()
                .any(|f| f.name() == "resolved_exclusive")
        );
        assert!(
            !RunFilterField::FIELDS
                .iter()
                .any(|f| f.name() == "exclusive")
        );
        assert!(
            QueueFilterField::FIELDS
                .iter()
                .any(|f| f.name() == "enqueued_at")
        );
        assert!(
            !QueueFilterField::FIELDS
                .iter()
                .any(|f| f.name() == "created_at")
        );
    }

    /// **The `$filter`/`$orderby` wire name is `environment_id`, on both
    /// collections, not `platform_id`.** Ruling G-3: the `OData` field used to
    /// diverge from the REST field of the same concept deliberately; that
    /// divergence is gone, and this is the test that would catch it coming
    /// back, either by a literal reverting or by a `#[serde]`-style rename
    /// shadowing it.
    #[test]
    fn the_environment_filter_field_is_named_environment_id_not_platform_id() {
        assert_eq!(RunFilterField::EnvironmentId.name(), "environment_id");
        assert!(
            !RunFilterField::FIELDS
                .iter()
                .any(|f| f.name() == "platform_id"),
            "no RunFilterField may advertise the pre-G-3 wire name"
        );
        assert_eq!(QueueFilterField::EnvironmentId.name(), "environment_id");
        assert!(
            !QueueFilterField::FIELDS
                .iter()
                .any(|f| f.name() == "platform_id"),
            "no QueueFilterField may advertise the pre-G-3 wire name"
        );

        // Minor-2 of the Task 25 review: the assertions above are about
        // *advertisement* (`name`/`FIELDS`). The CHANGELOG's claim is about
        // *refusal* - that a `$filter=platform_id eq ...` no longer parses -
        // which is decided by `FilterField::from_name`, not `FIELDS`.
        // `from_name` has a last-path-segment fallback the `FIELDS` scan
        // above never exercises, so it needs its own assertion.
        assert!(RunFilterField::from_name("platform_id").is_none());
        assert!(QueueFilterField::from_name("platform_id").is_none());
    }
}
