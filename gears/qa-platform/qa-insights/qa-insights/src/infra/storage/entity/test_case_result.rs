//! `SeaORM` entity for the `qa_test_case_results` table.
//!
//! Per-*function* outcomes parsed from the runner's `TEST_CASE` markers, one
//! row per test function per run, grouped under a file. Sibling of
//! [`super::test_result`], not its child: there is no foreign key in either
//! direction and none is wanted.
//!
//! Unlike `qa_test_results` this table carries **no** denormalized run columns.
//! Every case-level aggregate legacy computes reaches these rows through the
//! file-level table first, so a second copy of the run's identity would only be
//! a second thing to keep true.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qa_test_case_results")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub run_id: Uuid,
    pub test_file: String,
    pub nodeid: String,
    /// The test **function** name. Spelled `name`, not `test_name`, matching
    /// legacy's column (`001_initial.sql:258`) — `test_name` on
    /// [`super::test_result`] is the *file-level* display name, and the two are
    /// different values.
    pub name: String,
    pub status: String,
    pub duration: Option<String>,
    pub reason: Option<String>,
    pub ticket: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
