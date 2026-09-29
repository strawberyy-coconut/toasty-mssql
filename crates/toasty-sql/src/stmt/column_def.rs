use super::CheckConstraint;

use toasty_core::{
    driver::{self, Capability},
    schema::db::{self, Column},
    stmt::Expr,
};

/// A column definition used in `CREATE TABLE` and `ADD COLUMN` statements.
#[derive(Debug, Clone)]
pub struct ColumnDef {
    /// Column name.
    pub name: String,
    /// Storage type (e.g. `INTEGER`, `TEXT`).
    pub ty: db::Type,
    /// When `true`, the column has a `NOT NULL` constraint.
    pub not_null: bool,
    /// When `true`, the column auto-increments.
    pub auto_increment: bool,
    /// Optional CHECK constraint on this column.
    pub check: Option<CheckConstraint>,
}

impl ColumnDef {
    pub(crate) fn from_schema(
        column: &Column,
        storage_types: &driver::StorageTypes,
        capability: &Capability,
    ) -> Self {
        // For backends without a native enum type, store the variant name in
        // the backend's default string type with a CHECK constraint instead of
        // `db::Type::Enum`. The CHECK constraint restricts values to the
        // declared variants. SQLite's default string type is `TEXT`; SQL
        // Server's is a bounded `NVARCHAR`, which keeps the column indexable.
        if let db::Type::Enum(type_enum) = &column.storage_ty
            && !capability.native_enum
        {
            return Self {
                name: column.name.clone(),
                ty: storage_types.default_string_type.clone(),
                not_null: !column.nullable,
                auto_increment: column.auto_increment,
                check: Some(CheckConstraint {
                    name: None,
                    expr: Box::new(Expr::in_list(
                        Expr::Ident(column.name.clone()),
                        Expr::list(
                            type_enum
                                .variants
                                .iter()
                                .map(|v| Expr::from(v.name.clone())),
                        ),
                    )),
                }),
            };
        }

        Self {
            name: column.name.clone(),
            ty: column.storage_ty.clone(),
            not_null: !column.nullable,
            auto_increment: column.auto_increment,
            check: None,
        }
    }
}
