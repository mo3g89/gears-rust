//! `OData` field allow-lists and column mappings for this gear's paginated reads.
//!
//! # One enum, two consumers, and why that is the point
//!
//! Each `FilterField` enum here is passed to **both** the route
//! (`OperationBuilder::with_odata_filter::<F>()`, which is what publishes the
//! filterable fields in the `OpenAPI` document) and the repository
//! (`paginate_odata::<F, M, ..>`, which is what translates a filter into SQL).
//! Passing the same type to both is the only thing that stops the advertised
//! field set and the translatable field set drifting apart — a field advertised
//! but not mapped is a filter that fails at runtime, and a field mapped but not
//! advertised is a filter nobody knows exists. The shape is copied from
//! `qa-runs/src/infra/storage/odata.rs` rather than reinvented, so the two do
//! not drift either.
//!
//! # The allow-lists were chosen against the tables' actual indexes
//!
//! A field here is a promise: it is filterable, sortable, **and usable as a
//! pagination cursor**, which means the query planner has to cope with it and
//! the value has to be stable enough that a row the caller has already seen
//! cannot jump into a later page. Both tests are applied below, per field, and
//! the exclusions are written down because an absent field is the half a reader
//! cannot see.
//!
//! The indexes, read off the migrations rather than assumed:
//!
//! | table | index |
//! |---|---|
//! | `qa_environments` | `PRIMARY KEY (id)`; `UNIQUE idx_qa_environments_tenant_name (tenant_id, name)` |
//! | `qa_environment_variables` | `PRIMARY KEY (id)`; `UNIQUE idx_qa_environment_vars_tenant_unique (tenant_id, platform_id, name)`; `idx_qa_environment_vars_environment (platform_id)` |
//! | `qa_pipeline_variables` | `PRIMARY KEY (id)`; `UNIQUE idx_qa_pipeline_vars_unique (tenant_id, name)` |
//!
//! Every read on this gear's collections is `.secure().scope_with(scope)`d,
//! which puts `tenant_id` in the `WHERE` clause before anything the caller
//! sent. That is what makes `name` the useful sort key here rather than
//! `created_at`: with the tenant pinned, `(tenant_id, name)` is an exact index
//! prefix match, so the page's ordering, its cursor predicate and its `LIMIT`
//! all ride one index — and `name` is *unique per tenant*, which is what a
//! single-key cursor needs to be total. qa-runs sorts its history by
//! `created_at DESC` because a run history is a timeline; an environment
//! registry is a directory, and it is indexed like one.

use sea_orm::Value;
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};
use toolkit_odata::filter::{FieldKind, FilterField};

use crate::infra::storage::entity::environment::{
    Column as EnvironmentColumn, Entity as EnvironmentEntity, Model as EnvironmentModel,
};
use crate::infra::storage::entity::environment_variable::{
    Column as EnvironmentVarColumn, Entity as EnvironmentVarEntity,
    Model as EnvironmentVarModel,
};
use crate::infra::storage::entity::pipeline_variable::{
    Column as PipelineVarColumn, Entity as PipelineVarEntity, Model as PipelineVarModel,
};

/// Filterable, sortable and cursor-capable fields of the environment registry.
///
/// The tiebreaker is `name` ascending; see this module's header for why that
/// is the index-backed choice on this table, and
/// [`EnvironmentsRepository::list_page`](crate::domain::repos::EnvironmentsRepository::list_page)
/// for where it is applied.
///
/// # What is deliberately **not** here
///
/// * **`observed_attrs`, `config`, `credentials`** — JSON columns. No
///   comparison the `OData` translator emits means anything against one, and
///   `credentials` is the column whose whole design is that it structurally
///   cannot hold credential *material* (see its entity doc); making it a query
///   predicate would put it in the request surface anyway.
/// * **`description`, `health_detail`, `version_detect_error`** — free or
///   classified text, unindexed. This is the exclusion qa-insights'
///   `TestCaseResultsField` doc argues for `name`: admitting one would admit
///   `contains(description, …)` as a whole-tenant scan. The two diagnostic
///   fields are additionally rewritten by every observation cycle.
/// * **`updated_at`, `version_detected_at`, `health_checked_at`,
///   `health_state`** — all move on the background observation cycle, several
///   times an hour, without the row otherwise changing. A cursor keyed on one
///   would let an environment the caller has already seen reappear in a later
///   page. `health_state` is the tempting one — *"show me the down clusters"*
///   is a real operator question — and it is left out for exactly this reason;
///   it is a filter that wants a non-cursor mechanism, not this one.
/// * **`observed_build`, `observed_base_url`, `default_branch`, `available`** —
///   unindexed and nobody asks a *collection* question by them. An
///   unadvertised field can be added later; an advertised one is a wire
///   promise.
/// * **`tenant_id`** — never. It is the scope, decided by the PDP before the
///   query is built; advertising it would invite a caller to believe they can
///   choose one.
///
/// # What is here, and on what grounds
///
/// `id` and `name` ride the two indexes above. `product_id`, `is_default` and
/// `observed_version` are unindexed, and are admitted anyway on a bounded-size
/// argument rather than an index one: the collection they scan is *one
/// tenant's environments*, whose own NFR ceiling (`cpt-cf-qa-nfr-scale`) is 100
/// rows, and each is a filter the product UI needs — the environment picker
/// scopes by `product_id`, the Run and Schedule dialogs' "Default cluster"
/// resolves `is_default`, and the Analytics version filter reads
/// `observed_version`. All three are immutable or near-immutable per row
/// (`observed_version` changes on an upgrade, not on a poll), so all three are
/// sound cursor keys. `created_at` is immutable by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvironmentFilterField {
    Id,
    /// Unique per tenant (`idx_qa_environments_tenant_name`), which is what
    /// makes it usable as the sole cursor key.
    Name,
    ProductId,
    IsDefault,
    /// Nullable. `NULL` sorts as `SeaORM`/the backend sorts it; a filter is the
    /// intended use, not a sort.
    ObservedVersion,
    CreatedAt,
}

impl FilterField for EnvironmentFilterField {
    const FIELDS: &'static [Self] = &[
        Self::Id,
        Self::Name,
        Self::ProductId,
        Self::IsDefault,
        Self::ObservedVersion,
        Self::CreatedAt,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Name => "name",
            Self::ProductId => "product_id",
            Self::IsDefault => "is_default",
            Self::ObservedVersion => "observed_version",
            Self::CreatedAt => "created_at",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Id | Self::ProductId => FieldKind::Uuid,
            Self::Name | Self::ObservedVersion => FieldKind::String,
            Self::IsDefault => FieldKind::Bool,
            Self::CreatedAt => FieldKind::DateTimeUtc,
        }
    }
}

/// Column mapping for [`EnvironmentFilterField`].
pub struct EnvironmentODataMapper;

impl FieldToColumn<EnvironmentFilterField> for EnvironmentODataMapper {
    type Column = EnvironmentColumn;

    fn map_field(field: EnvironmentFilterField) -> EnvironmentColumn {
        match field {
            EnvironmentFilterField::Id => EnvironmentColumn::Id,
            EnvironmentFilterField::Name => EnvironmentColumn::Name,
            EnvironmentFilterField::ProductId => EnvironmentColumn::ProductId,
            EnvironmentFilterField::IsDefault => EnvironmentColumn::IsDefault,
            EnvironmentFilterField::ObservedVersion => EnvironmentColumn::ObservedVersion,
            EnvironmentFilterField::CreatedAt => EnvironmentColumn::CreatedAt,
        }
    }
}

impl ODataFieldMapping<EnvironmentFilterField> for EnvironmentODataMapper {
    type Entity = EnvironmentEntity;

    fn extract_cursor_value(model: &EnvironmentModel, field: EnvironmentFilterField) -> Value {
        match field {
            EnvironmentFilterField::Id => Value::Uuid(Some(Box::new(model.id))),
            EnvironmentFilterField::Name => Value::String(Some(Box::new(model.name.clone()))),
            EnvironmentFilterField::ProductId => Value::Uuid(Some(Box::new(model.product_id))),
            EnvironmentFilterField::IsDefault => Value::Bool(Some(model.is_default)),
            EnvironmentFilterField::ObservedVersion => {
                Value::String(model.observed_version.clone().map(Box::new))
            }
            EnvironmentFilterField::CreatedAt => {
                Value::TimeDateTimeWithTimeZone(Some(Box::new(model.created_at)))
            }
        }
    }
}

/// Filterable, sortable and cursor-capable fields of **both** variable
/// collections.
///
/// One enum for two tables, because `GET /qa/v1/variables` answers from both
/// and a caller writing a `$filter` cannot be asked which table their row will
/// come from. The field set is therefore the intersection of the two tables'
/// useful columns — `id`, `name`, `created_at` — and two mappers
/// ([`PipelineVarODataMapper`], [`EnvironmentVarODataMapper`]) translate it to
/// each table's own `Column`.
///
/// # What is deliberately **not** here
///
/// * **`value`** — the whole reason this enum is small. A variable value is
///   operator-supplied free text up to 64 KiB (`VariablesService`'s
///   `MAX_VALUE_BYTES`), unindexed on both tables, so `contains(value, …)` is
///   the whole-table scan qa-insights' `TestCaseResultsField` doc refuses. It
///   is also the one field here whose *content* is interesting: a filterable
///   `value` turns the collection into an oracle a caller could binary-search
///   with `startswith`, on rows they may read but would otherwise have to read
///   in full.
/// * **`environment_id`** — and this one is not an oversight, it is the
///   endpoint's design. Three reasons, in the order they decided it:
///   1. The column **does not exist on `qa_pipeline_variables` at all**, so a
///      single enum cannot map it for both halves of the union.
///   2. `/qa/v1/variables` already takes `environment_id` as a **first-class
///      query parameter**, and that parameter does something a `$filter` does
///      not: `VariablesService::list_for_env` runs a PLATFORM/`GET`-scoped
///      existence precheck on it and answers 404 for an environment the caller
///      cannot see. A `$filter` on the same concept would skip that check and
///      quietly answer an empty list instead — the existence-oracle shape this
///      crate already has a migration and two module docs about
///      (`m20260813_000005_tenant_scoped_variable_index`).
///   3. The **physical column is `platform_id`**
///      (`entity/environment_variable.rs`'s
///      `#[sea_orm(column_name = "platform_id")]`, kept deliberately: the
///      tables were renamed, the columns were not). `qa-runs`'
///      `RunFilterField::EnvironmentId` records the same trap on its own
///      table. Advertising neither spelling means there is no wire name that
///      can drift from the column —
///      `odata_tests::neither_enum_advertises_platform_id_or_environment_id`
///      pins that.
/// * **`updated_at`** — moves on every upsert, so a cursor keyed on it would
///   let a variable the caller has already seen reappear in a later page.
/// * **`tenant_id`** — never, for [`EnvironmentFilterField`]'s reason.
///
/// The tiebreaker is `name` ascending on both tables: with the tenant pinned by
/// the scope (and, on the per-environment table, `platform_id` pinned by the
/// repository), `name` is the residual column of each table's unique index and
/// is unique within that residual — an index-ordered, total cursor key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VariableFilterField {
    Id,
    Name,
    CreatedAt,
}

impl FilterField for VariableFilterField {
    const FIELDS: &'static [Self] = &[Self::Id, Self::Name, Self::CreatedAt];

    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Name => "name",
            Self::CreatedAt => "created_at",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Id => FieldKind::Uuid,
            Self::Name => FieldKind::String,
            Self::CreatedAt => FieldKind::DateTimeUtc,
        }
    }
}

/// [`VariableFilterField`] against `qa_pipeline_variables`.
pub struct PipelineVarODataMapper;

impl FieldToColumn<VariableFilterField> for PipelineVarODataMapper {
    type Column = PipelineVarColumn;

    fn map_field(field: VariableFilterField) -> PipelineVarColumn {
        match field {
            VariableFilterField::Id => PipelineVarColumn::Id,
            VariableFilterField::Name => PipelineVarColumn::Name,
            VariableFilterField::CreatedAt => PipelineVarColumn::CreatedAt,
        }
    }
}

impl ODataFieldMapping<VariableFilterField> for PipelineVarODataMapper {
    type Entity = PipelineVarEntity;

    fn extract_cursor_value(model: &PipelineVarModel, field: VariableFilterField) -> Value {
        match field {
            VariableFilterField::Id => Value::Uuid(Some(Box::new(model.id))),
            VariableFilterField::Name => Value::String(Some(Box::new(model.name.clone()))),
            VariableFilterField::CreatedAt => {
                Value::TimeDateTimeWithTimeZone(Some(Box::new(model.created_at)))
            }
        }
    }
}

/// [`VariableFilterField`] against `qa_environment_variables`.
pub struct EnvironmentVarODataMapper;

impl FieldToColumn<VariableFilterField> for EnvironmentVarODataMapper {
    type Column = EnvironmentVarColumn;

    fn map_field(field: VariableFilterField) -> EnvironmentVarColumn {
        match field {
            VariableFilterField::Id => EnvironmentVarColumn::Id,
            VariableFilterField::Name => EnvironmentVarColumn::Name,
            VariableFilterField::CreatedAt => EnvironmentVarColumn::CreatedAt,
        }
    }
}

impl ODataFieldMapping<VariableFilterField> for EnvironmentVarODataMapper {
    type Entity = EnvironmentVarEntity;

    fn extract_cursor_value(model: &EnvironmentVarModel, field: VariableFilterField) -> Value {
        match field {
            VariableFilterField::Id => Value::Uuid(Some(Box::new(model.id))),
            VariableFilterField::Name => Value::String(Some(Box::new(model.name.clone()))),
            VariableFilterField::CreatedAt => {
                Value::TimeDateTimeWithTimeZone(Some(Box::new(model.created_at)))
            }
        }
    }
}

/// The sort key and direction every paginated read in this gear defaults to.
///
/// One constant rather than three literals: the two variable reads and the
/// environment read must agree, because `VariablesService::list_for_env`
/// concatenates two of them into one response and a mismatched order would
/// interleave two differently-sorted runs. See this module's header for why
/// `name` and not `created_at`.
pub const NAME_TIEBREAKER: (&str, toolkit_odata::SortDir) = ("name", toolkit_odata::SortDir::Asc);
