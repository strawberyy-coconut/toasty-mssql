use super::expr::MssqlPredicate;
use super::{ColumnAlias, Comma, Delimited, Ident, ToSql};

use crate::{
    serializer::Dialect,
    stmt::{self, AlterColumnChanges, ColumnDef},
};
use toasty_core::{schema::db, stmt::SourceTableId};

struct ColumnsWithConstraints<'a>(&'a stmt::CreateTable);

impl ToSql for ColumnsWithConstraints<'_> {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        // SQLite needs the PK specified with the auto increment
        let trailing_pk = if f.serializer.is_sqlite() {
            // Sqlite only supports auto incrementing columns if they are the only primary key.
            match self.0.columns.iter().filter(|c| c.auto_increment).count() {
                0 => true,
                1 => {
                    // In this case, the primary key **must** be the auto incrementing column
                    let Some(pk) = self.0.primary_key.as_deref().and_then(|pk| pk.as_record())
                    else {
                        todo!("Toasty should catch this earlier")
                    };

                    let [stmt::Expr::Reference(pk)] = &pk.fields[..] else {
                        todo!("Toasty should catch this earlier")
                    };

                    let pk = pk.as_expr_column_unwrap();

                    assert_eq!(0, pk.nesting);
                    assert!(
                        self.0.columns[pk.column].auto_increment,
                        "Toasty should catch this earlier"
                    );

                    false
                }
                _ => panic!("Toasty should catch this case earlier"),
            }
        } else {
            true
        };

        let has_trailing_pk = self.0.primary_key.is_some() && trailing_pk;

        for (index, column) in self.0.columns.iter().enumerate() {
            fmt!(f, "\n    " column);
            if index < self.0.columns.len() - 1 || has_trailing_pk {
                fmt!(f, ",");
            }
        }

        match &self.0.primary_key {
            Some(pk) if trailing_pk => fmt!(f, "\n    PRIMARY KEY " pk "\n"),
            _ => fmt!(f, "\n"),
        }
    }
}

impl ToSql for &stmt::CreateIndex {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let index = f.serializer.index(self.index);
        let table = f.serializer.table(self.on);
        let index_name = Ident(&index.name);
        let table_name = Ident(&table.name);
        let columns = Comma(&self.columns);
        let unique = if self.unique { "UNIQUE " } else { "" };

        // Create a new expression scope to serialize the statement
        let mut f = f.scope(table);

        fmt!(
            &mut f, "CREATE " unique "INDEX " index_name " ON " table_name " (" columns ")"
        );

        // SQL Server counts `NULL`s as equal when enforcing a unique index, so a
        // second row with a `NULL` in a nullable unique column is rejected.
        // Every other backend Toasty supports treats `NULL`s as distinct, and
        // the planner relies on that, so the index is filtered to the rows where
        // the comparison is actually meaningful. This is the documented T-SQL
        // idiom for it.
        if f.serializer.is_mssql() && self.unique {
            let comparable: Vec<&str> = index
                .columns
                .iter()
                .map(|column| f.serializer.schema.column(column.column))
                .filter(|column| column.nullable)
                .map(|column| column.name.as_str())
                .collect();

            if !comparable.is_empty() {
                fmt!(&mut f, " WHERE ");
                for (position, name) in comparable.iter().enumerate() {
                    if position > 0 {
                        fmt!(&mut f, " AND ");
                    }
                    fmt!(&mut f, Ident(name) " IS NOT NULL");
                }
            }
        }
    }
}

impl ToSql for &stmt::AddColumn {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let table = f.serializer.table(self.table);
        let table_name = Ident(&table.name);

        // Create new expression scope to serialize the statement
        let mut f = f.scope(table);

        // T-SQL spells a column addition `ADD <def>`; the `COLUMN` keyword is a
        // syntax error there. (`DROP COLUMN` still takes it — see `DropColumn`.)
        let column_keyword = if f.serializer.is_mssql() {
            ""
        } else {
            "COLUMN "
        };

        fmt!(
            &mut f, "ALTER TABLE " table_name " ADD " column_keyword self.column
        );
    }
}

impl ToSql for &stmt::AlterColumn {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let table = f.serializer.table(self.id.table);
        let table_name = Ident(&table.name);

        // Create new expression scope to serialize the statement
        let mut f = f.scope(table);

        let column_name = Ident(&self.column_def.name);

        match f.serializer.dialect {
            Dialect::Postgresql => match &self.changes {
                AlterColumnChanges {
                    new_name: Some(name),
                    new_ty: None,
                    new_not_null: None,
                    new_auto_increment: None,
                } => {
                    fmt!(&mut f, "ALTER TABLE " table_name " RENAME COLUMN " column_name " TO " Ident(name.as_str()))
                }
                AlterColumnChanges {
                    new_name: None,
                    new_ty: Some(ty),
                    new_not_null: None,
                    new_auto_increment: None,
                } => {
                    fmt!(&mut f, "ALTER TABLE " table_name " ALTER COLUMN " column_name " TYPE " ty);

                    if let Some(intermediate_ty) =
                        enum_change_intermediate_ty(&self.column_def.ty, ty)
                    {
                        fmt!(
                            &mut f,
                            " USING " Ident(&self.column_def.name) "::" intermediate_ty "::" ty
                        );
                    }
                }
                AlterColumnChanges {
                    new_name: None,
                    new_ty: None,
                    new_not_null: Some(true),
                    new_auto_increment: None,
                } => {
                    fmt!(&mut f, "ALTER TABLE " table_name " ALTER COLUMN " column_name " SET NOT NULL")
                }
                AlterColumnChanges {
                    new_name: None,
                    new_ty: None,
                    new_not_null: Some(false),
                    new_auto_increment: None,
                } => {
                    fmt!(&mut f, "ALTER TABLE " table_name " ALTER COLUMN " column_name " DROP NOT NULL")
                }
                AlterColumnChanges {
                    new_name: None,
                    new_ty: None,
                    new_not_null: None,
                    new_auto_increment: Some(true),
                } => {
                    fmt!(&mut f, "ALTER TABLE " table_name " ALTER COLUMN " column_name " ADD GENERATED BY DEFAULT AS IDENTITY")
                }
                AlterColumnChanges {
                    new_name: None,
                    new_ty: None,
                    new_not_null: None,
                    new_auto_increment: Some(false),
                } => {
                    fmt!(&mut f, "ALTER TABLE " table_name " ALTER COLUMN " column_name " DROP IDENTITY")
                }
                _ => panic!(
                    "PostgreSQL does not support modifying multiple column properties in one ALTER TABLE statement"
                ),
            },
            Dialect::Mysql | Dialect::MariaDb => {
                let new_column_def = ColumnDef {
                    name: self
                        .changes
                        .new_name
                        .as_ref()
                        .unwrap_or(&self.column_def.name)
                        .clone(),
                    ty: self
                        .changes
                        .new_ty
                        .as_ref()
                        .unwrap_or(&self.column_def.ty)
                        .clone(),
                    not_null: self
                        .changes
                        .new_not_null
                        .unwrap_or(self.column_def.not_null),
                    auto_increment: self
                        .changes
                        .new_auto_increment
                        .unwrap_or(self.column_def.auto_increment),
                    check: self.column_def.check.clone(),
                };
                fmt!(&mut f, "ALTER TABLE " table_name " CHANGE COLUMN " column_name " " new_column_def)
            }
            Dialect::Sqlite => match &self.changes {
                AlterColumnChanges {
                    new_name: Some(name),
                    new_ty: None,
                    new_not_null: None,
                    new_auto_increment: None,
                } => {
                    fmt!(&mut f, "ALTER TABLE " table_name " RENAME COLUMN " column_name " TO " Ident(name.as_str()))
                }
                _ => panic!("SQLite only supports renaming columns in ALTER TABLE statement"),
            },
            // T-SQL renames a column with `sp_rename` rather than as part of
            // `ALTER TABLE`, and `ALTER COLUMN` always restates the whole
            // column — type and nullability — so a nullability-only change
            // fills the type in from the current definition.
            Dialect::Mssql => {
                if let Some(name) = &self.changes.new_name {
                    if self.changes.new_ty.is_none()
                        && self.changes.new_not_null.is_none()
                        && self.changes.new_auto_increment.is_none()
                    {
                        fmt!(
                            &mut f,
                            "EXEC sp_rename "
                            stmt::Value::String(format!("{}.{}", table.name, self.column_def.name))
                            ", "
                            stmt::Value::String(name.clone())
                            ", N'COLUMN'"
                        );
                    } else {
                        panic!(
                            "SQL Server does not support renaming a column in the same statement as another change"
                        );
                    }
                } else if self.changes.new_auto_increment.is_some() {
                    // `IDENTITY` is fixed at table creation; changing it means
                    // rebuilding the column, which a silent migration must not do.
                    panic!(
                        "T-SQL cannot change the identity property of `{}` in place",
                        self.column_def.name
                    );
                } else {
                    let ty = self.changes.new_ty.as_ref().unwrap_or(&self.column_def.ty);
                    let not_null = self
                        .changes
                        .new_not_null
                        .unwrap_or(self.column_def.not_null);

                    fmt!(&mut f, "ALTER TABLE " table_name " ALTER COLUMN " column_name " " ty);
                    if not_null {
                        fmt!(&mut f, " NOT NULL");
                    }
                }
            }
        }
    }
}

/// Returns the text type used to convert between distinct PostgreSQL enum types.
/// PostgreSQL does not define casts between separately declared enums, even when
/// their variants match, so `ALTER COLUMN ... TYPE` must cast each value through
/// its textual label. See <https://wiki.postgresql.org/wiki/Mass_type_replacement>.
fn enum_change_intermediate_ty(previous: &db::Type, next: &db::Type) -> Option<&'static str> {
    match (previous, next) {
        (db::Type::Enum(previous), db::Type::Enum(next)) if previous.name != next.name => {
            Some("TEXT")
        }
        (db::Type::List(previous), db::Type::List(next)) => {
            match (previous.as_ref(), next.as_ref()) {
                (db::Type::Enum(previous), db::Type::Enum(next)) if previous.name != next.name => {
                    Some("TEXT[]")
                }
                _ => None,
            }
        }
        _ => None,
    }
}

impl ToSql for &stmt::AlterTable {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match &self.action {
            stmt::AlterTableAction::RenameTo(new_name) => {
                // T-SQL renames a table — and a column — with `sp_rename`
                // rather than `ALTER TABLE … RENAME TO`.
                if f.serializer.is_mssql() {
                    fmt!(
                        f,
                        "EXEC sp_rename "
                        stmt::Value::String(self.name.0.join("."))
                        ", "
                        stmt::Value::String(new_name.0.join("."))
                    );
                    return;
                }

                fmt!(f, "ALTER TABLE " self.name " RENAME TO " new_name);
            }
        }
    }
}

impl ToSql for &stmt::CopyTable {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let target_cols = Comma(self.columns.iter().map(|(target, _)| target));
        let source_cols = Comma(self.columns.iter().map(|(_, source)| source));
        fmt!(f, "INSERT INTO " self.target " (" target_cols ") SELECT " source_cols " FROM " self.source);
    }
}

impl ToSql for &stmt::CreateTable {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let table = f.serializer.table(self.table);
        let name = Ident(&table.name);
        let columns = ColumnsWithConstraints(self);

        // Create new expression scope to serialize the statement
        let mut f = f.scope(table);

        fmt!(
            &mut f, "CREATE TABLE " name " (" columns ")"
        );
    }
}

impl ToSql for &stmt::CreateType {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        use toasty_core::stmt::Value;

        let name = self
            .ty
            .name
            .as_deref()
            .expect("CREATE TYPE requires a type name");
        let name = Ident(name);

        fmt!(f, "CREATE TYPE " name " AS ENUM (");
        for (i, variant) in self.ty.variants.iter().enumerate() {
            if i > 0 {
                f.dst.push_str(", ");
            }
            Value::String(variant.name.clone()).to_sql(f);
        }
        f.dst.push(')');
    }
}

impl ToSql for &stmt::AlterType {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        use toasty_core::stmt::Value;

        let name = Ident(&self.type_name);

        fmt!(f, "ALTER TYPE " name " ADD VALUE ");
        Value::String(self.variant.name.clone()).to_sql(f);
    }
}

impl ToSql for &stmt::RenameType {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        fmt!(
            f,
            "ALTER TYPE " Ident(&self.type_name) " RENAME TO " Ident(&self.new_name)
        );
    }
}

impl ToSql for &stmt::Delete {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        assert!(self.returning.is_none());

        // Conditions never reach the serializer: the planner rewrites a
        // conditional DELETE into a CTE or read-modify-write plan (see
        // `plan_conditional_sql_query_as_*`), stripping the condition and
        // folding its check into a filter predicate.
        debug_assert!(
            self.condition.is_none(),
            "SQL DELETE condition should have been lowered by the planner; condition={:#?}",
            self.condition
        );

        // T-SQL's `DELETE` names the table directly — no alias — and returns
        // rows with an `OUTPUT DELETED.<col>` clause placed before the `WHERE`.
        if f.serializer.is_mssql() {
            let table_id = delete_source_table(&self.from);
            let mut f = f.scope(self);
            f.alias = false;

            fmt!(&mut f, "DELETE FROM " f.serializer.table_name(table_id));
            if let Some(returning) = &self.returning {
                output_clause(&mut f, returning, "DELETED");
            }
            fmt!(&mut f, self.filter);
            return;
        }

        // Create a new expression scope to serialize the statement
        let mut f = f.scope(self);
        f.alias = true;

        fmt!(&mut f, "DELETE FROM " self.from self.filter);
    }
}

/// The single table a `DELETE`'s source names.
fn delete_source_table(from: &stmt::Source) -> db::TableId {
    let stmt::Source::Table(source) = from else {
        panic!("DELETE from a non-table source is not supported");
    };

    let [with_joins] = &source.from[..] else {
        panic!("DELETE with more than one table is not supported");
    };

    let stmt::TableFactor::Table(id) = &with_joins.relation;

    match &source.tables[id.0] {
        stmt::TableRef::Table(table_id) => *table_id,
        other => panic!("DELETE requires a plain table, found {other:?}"),
    }
}

/// Renders an `OUTPUT INSERTED.[a], …` / `OUTPUT DELETED.[a], …` clause.
fn output_clause(f: &mut super::Formatter<'_>, returning: &stmt::Returning, prefix: &'static str) {
    let previous = f.output;
    f.output = Some(prefix);
    fmt!(f, " OUTPUT " returning);
    f.output = previous;
}

impl ToSql for &stmt::Filter {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        if let Some(expr) = &self.expr {
            // T-SQL has no boolean expression type, so a filter that is a bare
            // `BIT` value has to be compared to `1`.
            if f.serializer.is_mssql() {
                fmt!(f, " WHERE " MssqlPredicate(expr));
            } else {
                fmt!(f, " WHERE " expr);
            }
        }
    }
}

impl ToSql for &stmt::Direction {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match self {
            stmt::Direction::Asc => fmt!(f, "ASC"),
            stmt::Direction::Desc => fmt!(f, "DESC"),
        }
    }
}

impl ToSql for &stmt::DropColumn {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let table = f.serializer.table(self.table);
        let table_name = Ident(&table.name);
        let if_exists = if self.if_exists { "IF EXISTS " } else { "" };

        // Create new expression scope to serialize the statement
        let mut f = f.scope(table);

        // T-SQL has no `DROP COLUMN IF EXISTS`, so the guard is spelled out.
        if self.if_exists && f.serializer.is_mssql() {
            fmt!(
                &mut f,
                "IF COL_LENGTH(" stmt::Value::String(table.name.clone()) ", "
                stmt::Value::String(self.name.0.join(".")) ") IS NOT NULL "
                "ALTER TABLE " table_name " DROP COLUMN " self.name
            );
            return;
        }

        fmt!(&mut f, "ALTER TABLE " table_name " DROP COLUMN " if_exists self.name);
    }
}

impl ToSql for &stmt::DropIndex {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let if_exists = if self.if_exists { "IF EXISTS " } else { "" };

        // T-SQL identifies an index by the table it is on, not by name alone.
        if f.serializer.is_mssql() {
            let on = self
                .on
                .expect("SQL Server requires the table a dropped index is on");
            fmt!(
                f,
                "DROP INDEX " if_exists self.name " ON " f.serializer.table_name(on)
            );
            return;
        }

        fmt!(f, "DROP INDEX " if_exists self.name);
    }
}

impl ToSql for &stmt::Pragma {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        if !f.serializer.is_sqlite() {
            panic!("\"PRAGMA\" statements only supported in SQLite");
        }
        match &self.value {
            Some(value) => fmt!(f, "PRAGMA " self.name.as_str() " = " value.as_str()),
            None => fmt!(f, "PRAGMA " self.name.as_str()),
        }
    }
}

impl ToSql for &stmt::DropTable {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let if_exists = if self.if_exists { "IF EXISTS " } else { "" };
        fmt!(f, "DROP TABLE " if_exists self.name);
    }
}

impl ToSql for &stmt::Insert {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        // Create a new expression scope to serialize the statement
        let mut f = f.scope(self);

        // T-SQL returns rows with an `OUTPUT` clause placed after the column
        // list, and expresses an upsert as a `MERGE`; both restructure the
        // statement rather than appending a clause, so they take their own path.
        if f.serializer.is_mssql() {
            insert_mssql(&mut f, self);
            return;
        }

        let returning = self
            .returning
            .as_ref()
            .map(|returning| (" RETURNING ", returning));

        if returning.is_some() && matches!(f.serializer.dialect, Dialect::Mysql) {
            panic!(
                "MySQL does not support the RETURNING clause with INSERT statements; returning={returning:#?}"
            );
        }

        f.in_insert = true;

        let upsert = self.upsert.as_ref().map(|_| UpsertClause(self));

        fmt!(
            &mut f, "INSERT INTO " self.target " " self.source upsert returning
        );
    }
}

/// Renders an `INSERT` as T-SQL: a plain `INSERT`, or a `MERGE` for an upsert.
fn insert_mssql(f: &mut super::Formatter<'_>, insert: &stmt::Insert) {
    let target = insert.target.as_table_unwrap();
    let table = f.serializer.table(target.table);

    // A generated column must be omitted from the target list entirely: T-SQL
    // rejects both `DEFAULT` and `NULL` as an explicit identity value.
    let keep: Vec<bool> = target
        .columns
        .iter()
        .map(|column| !table.columns[column.index].auto_increment)
        .collect();

    if insert.upsert.is_some() {
        insert_mssql_merge(f, insert, target, &keep);
        return;
    }

    fmt!(f, "INSERT INTO " f.serializer.table_name(target.table));

    let written: Vec<usize> = keep
        .iter()
        .enumerate()
        .filter_map(|(index, keep)| keep.then_some(index))
        .collect();

    if !written.is_empty() {
        fmt!(f, " (");
        for (position, index) in written.iter().enumerate() {
            if position > 0 {
                fmt!(f, ", ");
            }
            fmt!(f, f.serializer.column_name(target.columns[*index]));
        }
        fmt!(f, ")");
    }

    if let Some(returning) = &insert.returning {
        output_clause(f, returning, "INSERTED");
    }

    // When every target column is generated there is nothing to write, and
    // T-SQL spells that `DEFAULT VALUES` rather than empty parentheses.
    if written.is_empty() {
        fmt!(f, " DEFAULT VALUES");
        return;
    }

    fmt!(f, " ");

    let previous_columns = f.insert_columns.take();
    let previous_in_insert = f.in_insert;
    f.insert_columns = Some(keep);
    f.in_insert = true;
    fmt!(f, insert.source);
    f.in_insert = previous_in_insert;
    f.insert_columns = previous_columns;
}

/// Renders an upsert as `MERGE`.
///
/// `MERGE` is the only T-SQL statement that chooses between inserting and
/// updating *and* returns the resulting row from whichever branch ran, in one
/// round trip. `HOLDLOCK` is required: without it the engine can release the
/// range lock between the match test and the write, which is exactly the race
/// an upsert exists to avoid.
fn insert_mssql_merge(
    f: &mut super::Formatter<'_>,
    insert: &stmt::Insert,
    target: &stmt::InsertTable,
    keep: &[bool],
) {
    let upsert = insert.upsert.as_deref().expect("the caller checked");

    let stmt::UpsertTarget::Columns(conflict) = &upsert.target else {
        panic!("upsert target must be lowered before SQL serialization")
    };

    let stmt::ExprSet::Values(rows) = &insert.source.body else {
        panic!("SQL Server requires an upsert source to be a list of values")
    };

    let previous_merge = f.merge;
    f.merge = true;

    fmt!(
        f,
        "MERGE INTO " f.serializer.table_name(target.table) " WITH (HOLDLOCK) AS target"
    );

    // `USING (VALUES …) AS src (…)` — the rows carry the create branch's
    // values, so the insert branch needs no expressions of its own.
    fmt!(f, " USING (VALUES ");
    for (index, row) in rows.rows.iter().enumerate() {
        if index > 0 {
            fmt!(f, ", ");
        }

        let stmt::Expr::Record(record) = row else {
            panic!("SQL Server requires an upsert row to be a record");
        };

        fmt!(f, "(");
        for (position, _column) in target.columns.iter().enumerate() {
            if position > 0 {
                fmt!(f, ", ");
            }

            match upsert_assignment(position, &[&upsert.create]) {
                Some(stmt::Assignment::Set(expr)) => fmt!(f, expr),
                Some(other) => {
                    panic!("SQL Server cannot express {other:?} for a column an upsert creates")
                }
                None => {
                    let field = record
                        .fields
                        .get(position)
                        .unwrap_or_else(|| panic!("upsert row shorter than its column list"));
                    fmt!(f, field);
                }
            }
        }
        fmt!(f, ")");
    }
    fmt!(f, ") AS src (");
    for (position, column) in target.columns.iter().enumerate() {
        if position > 0 {
            fmt!(f, ", ");
        }
        fmt!(f, f.serializer.column_name(*column));
    }
    fmt!(f, ")");

    // The conflict target is matched on exactly the lowered columns, so a
    // conflict on some *other* unique constraint still raises.
    fmt!(f, " ON ");
    for (index, column) in conflict.iter().enumerate() {
        if index > 0 {
            fmt!(f, " AND ");
        }
        let name = f.serializer.schema.column(*column).name.as_str();
        fmt!(f, "target." Ident(name) " = src." Ident(name));
    }

    // An ignored conflict runs no action at all, which is T-SQL's `DO NOTHING`.
    if matches!(upsert.action, stmt::UpsertAction::Update) {
        fmt!(f, " WHEN MATCHED THEN UPDATE SET ");

        let mut written = 0;
        for (position, column) in target.columns.iter().enumerate() {
            let Some(assignment) = upsert_assignment(
                position,
                &[&upsert.update_defaults, &upsert.shared, &upsert.update],
            ) else {
                continue;
            };

            if written > 0 {
                fmt!(f, ", ");
            }
            written += 1;

            let name = f.serializer.schema.column(*column).name.as_str();
            fmt!(f, "target." Ident(name) " = ");
            serialize_assignment(f, AssignmentColumn(*column), assignment);
        }

        if written == 0 {
            panic!(
                "SQL Server requires an upsert that updates a conflicting row to assign something"
            )
        }
    }

    fmt!(f, " WHEN NOT MATCHED THEN INSERT (");
    let mut written = 0;
    for (position, column) in target.columns.iter().enumerate() {
        if !keep[position] {
            continue;
        }
        if written > 0 {
            fmt!(f, ", ");
        }
        written += 1;
        fmt!(f, f.serializer.column_name(*column));
    }
    fmt!(f, ") VALUES (");
    let mut written = 0;
    for (position, column) in target.columns.iter().enumerate() {
        if !keep[position] {
            continue;
        }
        if written > 0 {
            fmt!(f, ", ");
        }
        written += 1;
        let name = f.serializer.schema.column(*column).name.as_str();
        fmt!(f, "src." Ident(name));
    }
    fmt!(f, ")");

    if let Some(returning) = &insert.returning {
        output_clause(f, returning, "INSERTED");
    }

    // T-SQL insists a `MERGE` end with a semicolon; `Serializer::serialize`
    // appends one for every statement, so no extra terminator is emitted here.

    f.merge = previous_merge;
}

/// The assignment an upsert makes to one column, looking through `groups` in
/// priority order: the last group that names the column wins.
fn upsert_assignment<'a>(
    column: usize,
    groups: &[&'a stmt::Assignments],
) -> Option<&'a stmt::Assignment> {
    let key = stmt::Projection::single(column);
    groups.iter().rev().find_map(|group| group.get(&key))
}

/// Renders one assignment against an existing column.
fn serialize_assignment(
    f: &mut super::Formatter<'_>,
    existing_column: AssignmentColumn,
    assignment: &stmt::Assignment,
) {
    match assignment {
        stmt::Assignment::Set(expr) => fmt!(f, expr),
        stmt::Assignment::Append(expr) => serialize_append(f, existing_column, expr),
        stmt::Assignment::Add(expr) => fmt!(f, existing_column " + " expr),
        stmt::Assignment::Subtract(expr) => fmt!(f, existing_column " - " expr),
        other => panic!("SQL Server does not support the assignment {other:?}"),
    }
}

struct UpsertClause<'a>(&'a stmt::Insert);

impl ToSql for UpsertClause<'_> {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let insert = self.0;
        let upsert = insert.upsert.as_ref().unwrap();
        let target = insert.target.as_table_unwrap();
        let stmt::UpsertTarget::Columns(columns) = &upsert.target else {
            panic!("upsert target must be lowered before SQL serialization")
        };
        let columns = Comma(
            columns
                .iter()
                .map(|column| f.serializer.column_name(*column)),
        );

        fmt!(f, " ON CONFLICT (" columns ")");
        match upsert.action {
            stmt::UpsertAction::Ignore => fmt!(f, " DO NOTHING"),
            stmt::UpsertAction::Update => {
                let table = f.serializer.table(target.table);
                let assignments = AssignmentList::upsert(table, &upsert.shared);
                fmt!(f, " DO UPDATE SET " assignments);
            }
        }
    }
}

impl ToSql for &stmt::InsertTarget {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match self {
            stmt::InsertTarget::Table(insert_table) => {
                let table_name = f.serializer.table_name(insert_table);
                let columns = Comma(
                    insert_table
                        .columns
                        .iter()
                        .map(|column_id| f.serializer.column_name(*column_id)),
                );

                fmt!(f, table_name " (" columns ")");
            }
            _ => todo!("self={self:?}"),
        }
    }
}

impl ToSql for &stmt::Limit {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        // T-SQL spells pagination `OFFSET … ROWS FETCH NEXT … ROWS ONLY`. Both
        // pagination strategies come down to the same clause: the engine lowers
        // a cursor into an ordinary filter before the statement reaches the
        // driver, so a cursor page is an offset page with no offset.
        if f.serializer.is_mssql() {
            let (offset, fetch) = match self {
                stmt::Limit::Cursor(cursor) => {
                    assert!(
                        cursor.after.is_none(),
                        "Limit::Cursor with after cannot be serialized to SQL, should already be lowered"
                    );
                    (None, &cursor.page_size)
                }
                stmt::Limit::Offset(limit_offset) => {
                    (limit_offset.offset.as_ref(), &limit_offset.limit)
                }
            };

            fmt!(f, "OFFSET ");
            match offset {
                Some(offset) => fmt!(f, offset),
                None => fmt!(f, 0usize),
            }
            fmt!(f, " ROWS FETCH NEXT " fetch " ROWS ONLY");
            return;
        }

        match self {
            stmt::Limit::Cursor(cursor) => {
                assert!(
                    cursor.after.is_none(),
                    "Limit::Cursor with after cannot be serialized to SQL, should already be lowered"
                );
                fmt!(f, "LIMIT " cursor.page_size);
            }
            stmt::Limit::Offset(limit_offset) => {
                fmt!(f, "LIMIT " limit_offset.limit);
                if let Some(offset) = limit_offset.offset.as_ref() {
                    fmt!(f, " OFFSET " offset);
                }
            }
        }
    }
}

impl ToSql for &stmt::Query {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        // Create a new expression scope to serialize the statement
        let mut f = f.scope(self);
        f.alias = true;

        // T-SQL spells `FOR UPDATE` as a table hint on the source, not as a
        // trailing clause, so the lock has to be known before the body renders.
        if f.serializer.is_mssql() {
            f.row_lock = self
                .locks
                .iter()
                .any(|lock| matches!(lock, stmt::Lock::Update));

            if let Some(with) = &self.with {
                fmt!(&mut f, with " ");
            }

            fmt!(&mut f, self.body);

            // The hint belongs to this query's own `FROM`; clearing it here
            // keeps a nested query's source from inheriting it.
            f.row_lock = false;

            if let Some(order_by) = &self.order_by {
                fmt!(&mut f, " " order_by);
            } else if self.limit.is_some() {
                // `OFFSET … FETCH NEXT` is only valid after an `ORDER BY`.
                fmt!(&mut f, " ORDER BY (SELECT NULL)");
            }

            if let Some(limit) = &self.limit {
                fmt!(&mut f, " " limit);
            }

            return;
        }

        let locks = if self.locks.is_empty() {
            None
        } else {
            Some((" ", Delimited(&self.locks, " ")))
        };

        let body = &self.body;
        let order_by = self.order_by.as_ref().map(|order_by| (" ", order_by));
        let limit = self.limit.as_ref().map(|limit| (" ", limit));

        fmt!(&mut f, self.with body order_by limit locks);
    }
}

impl ToSql for &stmt::ExprSet {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match self {
            stmt::ExprSet::Select(expr) => expr.to_sql(f),
            stmt::ExprSet::Values(expr) => expr.to_sql(f),
            stmt::ExprSet::Update(expr) => expr.to_sql(f),
            stmt::ExprSet::Delete(expr) => expr.to_sql(f),
            _ => todo!("self={self:?}"),
        }
    }
}

impl ToSql for &stmt::OrderBy {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let order_by = Comma(&self.exprs);

        fmt!(f, "ORDER BY " order_by);
    }
}

impl ToSql for &stmt::OrderByExpr {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        if let Some(order) = &self.order {
            fmt!(f, self.expr " " order);
        } else {
            fmt!(f, self.expr);
        }
    }
}

impl ToSql for &stmt::Returning {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match self {
            stmt::Returning::Project(stmt::Expr::Record(expr_record)) => {
                // An `OUTPUT` clause accepts no aliases, so SQL Server lists the
                // fields bare.
                if f.output.is_some() {
                    fmt!(f, Comma(&expr_record.fields));
                    return;
                }

                // Alias every projected field positionally (`AS column1`, ...).
                // A nested SELECT/RETURNING referenced from an outer query (e.g.
                // a data-modifying CTE joined for its returned rows) is read by
                // that alias — `ColumnAlias` — so a bare column reference must
                // carry it too, not just computed expressions. Drivers read
                // top-level results positionally, so the alias is harmless there.
                let fields = expr_record
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, expr)| (expr, Some(" AS "), Some(ColumnAlias(i))));

                fmt!(f, Comma(fields));
            }
            stmt::Returning::Project(stmt::Expr::Value(stmt::Value::Record(value_record))) => {
                fmt!(f, Comma(&value_record.fields));
            }
            _ => todo!("returning={self:#?}"),
        }
    }
}

impl ToSql for &stmt::Select {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let source_table = self.source.as_table_unwrap();
        let select = if self.distinct {
            "SELECT DISTINCT "
        } else {
            "SELECT "
        };

        if source_table.from.is_empty() {
            fmt!(f, select self.returning)
        } else {
            fmt!(f, select self.returning " FROM " self.source self.filter);
        }
    }
}

impl ToSql for &stmt::Lock {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match self {
            stmt::Lock::Update => fmt!(f, "FOR UPDATE"),
            stmt::Lock::Share => fmt!(f, "FOR SHARE"),
        }
    }
}

impl ToSql for &stmt::Source {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match self {
            stmt::Source::Table(source_table) => {
                source_table.to_sql(f);
            }
            _ => todo!("self={self:?}"),
        }
    }
}

impl ToSql for &toasty_core::stmt::Statement {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        use toasty_core::stmt::Statement::*;

        f.depth += 1;

        match self {
            Delete(stmt) => stmt.to_sql(f),
            Insert(stmt) => stmt.to_sql(f),
            Query(stmt) => stmt.to_sql(f),
            Update(stmt) => stmt.to_sql(f),
        }

        f.depth -= 1;
    }
}

impl ToSql for &stmt::Statement {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match self {
            stmt::Statement::AddColumn(stmt) => stmt.to_sql(f),
            stmt::Statement::AlterColumn(stmt) => stmt.to_sql(f),
            stmt::Statement::AlterTable(stmt) => stmt.to_sql(f),
            stmt::Statement::AlterType(stmt) => stmt.to_sql(f),
            stmt::Statement::CopyTable(stmt) => stmt.to_sql(f),
            stmt::Statement::CreateIndex(stmt) => stmt.to_sql(f),
            stmt::Statement::CreateTable(stmt) => stmt.to_sql(f),
            stmt::Statement::CreateType(stmt) => stmt.to_sql(f),
            stmt::Statement::DropColumn(stmt) => stmt.to_sql(f),
            stmt::Statement::DropIndex(stmt) => stmt.to_sql(f),
            stmt::Statement::DropTable(stmt) => stmt.to_sql(f),
            stmt::Statement::Pragma(stmt) => stmt.to_sql(f),
            stmt::Statement::RenameType(stmt) => stmt.to_sql(f),
            stmt::Statement::Delete(stmt) => stmt.to_sql(f),
            stmt::Statement::Insert(stmt) => stmt.to_sql(f),
            stmt::Statement::Query(stmt) => stmt.to_sql(f),
            stmt::Statement::Update(stmt) => stmt.to_sql(f),
        }
    }
}

impl ToSql for &stmt::SourceTable {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        // Iterate over each TableWithJoins in the from clause
        for (i, table_with_joins) in self.from.iter().enumerate() {
            if i > 0 {
                fmt!(f, ", ");
            }

            // Serialize the main table relation
            match &table_with_joins.relation {
                stmt::TableFactor::Table(table_id) => {
                    let table_ref = &self.tables[table_id.0];
                    let alias = TableAlias {
                        depth: f.depth,
                        table: *table_id,
                    };

                    fmt!(f, table_ref " AS " alias);

                    // T-SQL requires a `VALUES`-derived table to name its
                    // columns, and a locking read is a table hint placed on the
                    // query's own `FROM` item rather than a trailing clause.
                    if f.serializer.is_mssql() {
                        mssql_derived_column_list(f, table_ref);
                        if f.row_lock {
                            fmt!(f, " WITH (UPDLOCK, ROWLOCK)");
                            f.row_lock = false;
                        }
                    }
                }
            }

            // Serialize the joins
            for join in &table_with_joins.joins {
                let (kw, expr) = match &join.constraint {
                    stmt::JoinOp::Inner(expr) => (" INNER JOIN ", expr),
                    stmt::JoinOp::Left(expr) => (" LEFT JOIN ", expr),
                };
                let join_table_ref = &self.tables[join.table.0];
                let alias = TableAlias {
                    depth: f.depth,
                    table: join.table,
                };
                fmt!(f, kw join_table_ref " AS " alias);
                if f.serializer.is_mssql() {
                    mssql_derived_column_list(f, join_table_ref);
                    fmt!(f, " ON " MssqlPredicate(expr));
                } else {
                    fmt!(f, " ON " expr);
                }
            }
        }
    }
}

/// Names the columns of a `VALUES`-derived table.
///
/// Postgres and SQLite auto-name those columns, but T-SQL requires them to be
/// listed, and outer references address them by exactly these positional names.
fn mssql_derived_column_list(f: &mut super::Formatter<'_>, table_ref: &stmt::TableRef) {
    let stmt::TableRef::Derived(derived) = table_ref else {
        return;
    };

    let stmt::ExprSet::Values(values) = &derived.subquery.body else {
        return;
    };

    let Some(first) = values.rows.first() else {
        return;
    };

    let fields = match first {
        stmt::Expr::Record(record) => record.fields.len(),
        _ => 1,
    };

    fmt!(f, " (");
    for column in 0..fields {
        if column > 0 {
            fmt!(f, ", ");
        }
        fmt!(f, mssql_column_alias_name(column));
    }
    fmt!(f, ")");
}

/// The positional name T-SQL gives the `index`th column of a derived table.
fn mssql_column_alias_name(index: usize) -> String {
    format!("column{}", index + 1)
}

impl ToSql for &stmt::TableRef {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match self {
            stmt::TableRef::Table(table_id) => {
                let table_name = f.serializer.table_name(*table_id);
                fmt!(f, table_name);
            }
            stmt::TableRef::Derived(table_derived) => fmt!(f, table_derived),
            stmt::TableRef::Cte { nesting, index } => {
                assert!(f.depth >= *nesting, "nesting={nesting} depth={}", f.depth);

                let depth = f.depth - nesting;
                fmt!(f, "cte_" depth "_" index);
            }
            stmt::TableRef::Arg(..) => panic!("unexpected TableRef argument; table_ref={self:#?}"),
        }
    }
}

impl ToSql for &stmt::TableDerived {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        debug_assert!(f.alias);

        f.depth += 1;
        fmt!(f, "(" self.subquery ")");
        f.depth -= 1;
    }
}

struct TableAlias {
    depth: usize,
    table: SourceTableId,
}

impl ToSql for &TableAlias {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        fmt!(f, "tbl_" self.depth "_" self.table.0);
    }
}

impl ToSql for &stmt::Update {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let table = f.serializer.schema.table(self.target.as_table_unwrap());
        let assignments = AssignmentList::update(table, &self.assignments);

        // Create a new expression scope to serialize the statement
        let mut f = f.scope(self);
        f.alias = false;

        let returning = self
            .returning
            .as_ref()
            .map(|returning| (" RETURNING ", returning));

        if returning.is_some() && f.serializer.is_mysql() {
            panic!(
                "MySQL does not support the RETURNING clause with UPDATE statements; returning={returning:#?}"
            );
        }

        // Conditions never reach the serializer: the planner rewrites a
        // conditional UPDATE into a CTE or read-modify-write plan (see
        // `plan_conditional_sql_query_as_*`), stripping the condition and
        // folding its check into a filter predicate.
        debug_assert!(
            self.condition.is_none(),
            "SQL UPDATE condition should have been lowered by the planner; condition={:#?}",
            self.condition
        );

        // T-SQL's `UPDATE` names the table directly — no alias — and returns
        // rows with an `OUTPUT INSERTED.<col>` clause placed after the `SET`
        // list and before the `WHERE`.
        if f.serializer.is_mssql() {
            fmt!(&mut f, "UPDATE " f.serializer.table_name(self.target.as_table_unwrap()));
            fmt!(&mut f, " SET " assignments);
            if let Some(returning) = &self.returning {
                output_clause(&mut f, returning, "INSERTED");
            }
            fmt!(&mut f, self.filter);
            return;
        }

        fmt!(&mut f, "UPDATE " self.target " SET " assignments self.filter returning);
    }
}

struct AssignmentList<'a> {
    table: &'a db::Table,
    assignments: &'a stmt::Assignments,
    qualify_existing: bool,
}

impl<'a> AssignmentList<'a> {
    fn update(table: &'a db::Table, assignments: &'a stmt::Assignments) -> Self {
        Self {
            table,
            assignments,
            qualify_existing: false,
        }
    }

    fn upsert(table: &'a db::Table, assignments: &'a stmt::Assignments) -> Self {
        Self {
            table,
            assignments,
            qualify_existing: true,
        }
    }
}

#[derive(Clone, Copy)]
struct AssignmentColumn(db::ColumnId);

impl ToSql for AssignmentColumn {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        // Inside a `MERGE`, the source relation carries the same column names
        // as the written table, so the written table is qualified.
        if f.merge {
            fmt!(f, "target." f.serializer.column_name(self.0))
        } else if matches!(f.serializer.dialect, Dialect::Postgresql)
            && f.assignment_table == Some(self.0.table)
        {
            fmt!(f, f.serializer.table_name(self.0.table) "." f.serializer.column_name(self.0))
        } else {
            fmt!(f, f.serializer.column_name(self.0))
        }
    }
}

impl ToSql for AssignmentList<'_> {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        let previous_assignment_table = f.assignment_table;
        f.assignment_table = self.qualify_existing.then_some(self.table.id);
        let assignments: Vec<_> = self.assignments.iter().collect();

        for (i, (projection, assignment)) in assignments.iter().enumerate() {
            if i > 0 {
                f.dst.push_str(", ");
            }

            let column = self.table.resolve(projection);
            let column_name = Ident(&column.name);
            let existing_column = AssignmentColumn(column.id);

            // Serialize column name and equals sign
            column_name.to_sql(f);
            f.dst.push_str(" = ");

            match assignment {
                stmt::Assignment::Set(expr) => expr.to_sql(f),
                stmt::Assignment::Append(expr) => {
                    serialize_append(f, existing_column, expr);
                }
                stmt::Assignment::Remove(expr) => {
                    serialize_remove(f, existing_column, expr);
                }
                stmt::Assignment::Pop => {
                    serialize_pop(f, existing_column);
                }
                stmt::Assignment::RemoveAt(expr) => {
                    serialize_remove_at(f, existing_column, expr);
                }
                stmt::Assignment::Add(expr) => {
                    fmt!(f, existing_column " + " expr);
                }
                stmt::Assignment::Subtract(expr) => {
                    fmt!(f, existing_column " - " expr);
                }
                _ => todo!(
                    "only SET / APPEND / REMOVE / POP / REMOVE_AT / ADD / SUBTRACT supported in SQL serialization; got {assignment:#?}"
                ),
            }
        }

        f.assignment_table = previous_assignment_table;
    }
}

/// Emit a backend-specific append expression for an `Assignment::Append`.
///
/// The right-hand side is a list expression (PG `text[]` bind for native
/// arrays, JSON string bind for MySQL `JSON` and SQLite JSON1). The
/// serializer renders the dialect's native append operator:
///
/// - PostgreSQL: `col || $1` — `text[] || text[]` concatenates arrays.
///   During an upsert, `col` is table-qualified to distinguish the stored
///   value from `excluded.col`.
/// - MySQL: `JSON_MERGE_PRESERVE(col, $1)` — preserves duplicates and
///   appends every element of the right-hand array to the left-hand one.
/// - SQLite: a scalar expression removes the closing bracket from the stored
///   array and the opening bracket from the appended array, then joins them
///   with a comma when both contain elements. `json()` validates and
///   canonicalizes the result. This form also works in SQLite-compatible
///   engines that reject subqueries inside an upsert assignment.
fn serialize_append(f: &mut super::Formatter<'_>, column: AssignmentColumn, expr: &stmt::Expr) {
    match f.serializer.dialect {
        Dialect::Postgresql => fmt!(f, column " || " expr),
        Dialect::Mysql | Dialect::MariaDb => {
            fmt!(f, "JSON_MERGE_PRESERVE(" column ", " expr ")")
        }
        Dialect::Sqlite => fmt!(
            f,
            "json(substr(" column ", 1, length(" column ") - 1) || \
             CASE WHEN json_array_length(" column ") > 0 \
             AND json_array_length(" expr ") > 0 THEN ',' ELSE '' END || \
             substr(" expr ", 2))"
        ),
        // T-SQL before SQL Server 2025 cannot combine two JSON arrays with a
        // function, so the two canonical texts are spliced: drop the stored
        // array's closing bracket and the incoming array's opening bracket,
        // then join with a comma. `DATALENGTH(...) / 2` counts UTF-16 code
        // units with no trailing-blank ambiguity of the kind `LEN` has.
        Dialect::Mssql => fmt!(
            f,
            "CASE WHEN " column " = N'[]' THEN " expr
            " WHEN " expr " = N'[]' THEN " column
            " ELSE SUBSTRING(" column ", 1, DATALENGTH(" column ") / 2 - 1) + N',' + \
             SUBSTRING(" expr ", 2, DATALENGTH(" expr ") / 2 - 1) END"
        ),
    }
}

/// Emit `stmt::remove(value)` against a `Vec<scalar>` column.
///
/// - PostgreSQL: `array_remove(col, $value)` — removes every element equal
///   to `$value` from a `T[]` column. Atomic.
/// - MySQL / SQLite: not yet supported — `vec_remove` is gated off and the
///   lowering rejects these backends before reaching here.
fn serialize_remove(f: &mut super::Formatter<'_>, column: AssignmentColumn, expr: &stmt::Expr) {
    match f.serializer.dialect {
        Dialect::Postgresql => fmt!(f, "array_remove(" column ", " expr ")"),
        Dialect::Mysql | Dialect::MariaDb | Dialect::Sqlite | Dialect::Mssql => panic!(
            "stmt::remove on a Vec<scalar> field is not yet implemented for this SQL dialect; \
             the lowering should have rejected this before reaching the serializer",
        ),
    }
}

/// Emit `stmt::pop()` against a `Vec<scalar>` column.
///
/// - PostgreSQL: `col[1:cardinality(col) - 1]` — slices off the last
///   element via 1-based PG array slicing. Atomic.
/// - MySQL / SQLite: not yet supported — `vec_pop` is gated off and the
///   lowering rejects these backends before reaching here.
fn serialize_pop(f: &mut super::Formatter<'_>, column: AssignmentColumn) {
    match f.serializer.dialect {
        Dialect::Postgresql => fmt!(f, column "[1:cardinality(" column ") - 1]"),
        Dialect::Mysql | Dialect::MariaDb | Dialect::Sqlite | Dialect::Mssql => panic!(
            "stmt::pop on a Vec<scalar> field is not yet implemented for this SQL dialect; \
             the lowering should have rejected this before reaching the serializer",
        ),
    }
}

/// Emit `stmt::remove_at(idx)` against a `Vec<scalar>` column.
///
/// `idx` is a 0-based `usize`. PostgreSQL arrays are 1-based, so the
/// element at user-facing index `i` lives at PG position `i + 1`. The
/// expression `col[1:i] || col[i + 2:cardinality(col)]` keeps the prefix
/// up to (1-based) position `i` and the suffix from position `i + 2`,
/// dropping the element at position `i + 1` (user index `i`).
///
/// Out-of-bounds indices are a no-op: when `i >= cardinality(col)`, the
/// first slice yields the entire array and the second yields the empty
/// slice, so the concatenation reproduces the input.
///
/// - MySQL / SQLite: not yet supported — `vec_remove_at` is gated off
///   and the lowering rejects these backends before reaching here.
fn serialize_remove_at(f: &mut super::Formatter<'_>, column: AssignmentColumn, expr: &stmt::Expr) {
    match f.serializer.dialect {
        Dialect::Postgresql => fmt!(
            f,
            column "[1:" expr "] || " column "[" expr " + 2:cardinality(" column ")]"
        ),
        Dialect::Mysql | Dialect::MariaDb | Dialect::Sqlite | Dialect::Mssql => panic!(
            "stmt::remove_at on a Vec<scalar> field is not yet implemented for this SQL dialect; \
             the lowering should have rejected this before reaching the serializer",
        ),
    }
}

impl ToSql for &stmt::UpdateTarget {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match self {
            stmt::UpdateTarget::Table(table_id) => {
                let table_name = f.serializer.table_name(*table_id);
                let alias = TableAlias {
                    depth: f.depth,
                    table: SourceTableId(0),
                };

                fmt!(f, table_name " AS " alias);
            }
            _ => todo!(),
        }
    }
}

impl ToSql for &stmt::Values {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        // MySQL requires the `ROW(...)` keyword for table value constructors
        // when used in subqueries, but NOT in INSERT statements. MariaDB has
        // no `VALUES ROW(...)` at all and takes the plain form below.
        //
        // Rows are `Expr::Record`s, which serialize with their own `(...)`.
        // Inside `ROW(...)` we need the fields comma-separated *without*
        // those parens — otherwise MySQL parses `ROW((1, 'a'))` as a single
        // row-expression operand and rejects it with "Operand should contain
        // 1 column(s)".
        if matches!(f.serializer.dialect, Dialect::Mysql) && !f.in_insert {
            // `Expr::Record` serializes with its own `(...)`; render its
            // fields directly inside `ROW(...)` so we don't end up with
            // `ROW((a, b))` (which MySQL parses as a single row-typed
            // operand and rejects). Other expression shapes are wrapped
            // as-is — a single-scalar row `ROW(x)` is well-formed.
            for (i, row) in self.rows.iter().enumerate() {
                if i == 0 {
                    fmt!(f, "VALUES ");
                } else {
                    fmt!(f, ", ");
                }
                match row {
                    stmt::Expr::Record(record) => {
                        fmt!(f, "ROW(" Comma(record.fields.iter()) ")")
                    }
                    _ => fmt!(f, "ROW(" row ")"),
                }
            }
        } else if matches!(f.serializer.dialect, Dialect::MariaDb) && !f.in_insert {
            // MariaDB's table value constructor cannot type a bare `?`: the
            // column binds to the empty string, so the join silently matches
            // nothing. A UNION ALL of SELECTs binds and names its own columns.
            let rows = self.rows.iter().enumerate().map(|(i, row)| {
                let fields = match row {
                    stmt::Expr::Record(record) => &record.fields[..],
                    other => std::slice::from_ref(other),
                };
                // Only the first SELECT names the columns.
                let fields = fields.iter().enumerate().map(move |(column, field)| {
                    (field, (i == 0).then_some((" AS ", ColumnAlias(column))))
                });
                ("SELECT ", Comma(fields))
            });
            fmt!(f, Delimited(rows, " UNION ALL "));
        } else if f.serializer.is_mssql()
            && let Some(keep) = f.insert_columns.clone()
        {
            // An INSERT whose target list dropped generated columns also drops
            // those fields from each row.
            fmt!(f, "VALUES ");
            for (index, row) in self.rows.iter().enumerate() {
                if index > 0 {
                    fmt!(f, ", ");
                }

                match row {
                    stmt::Expr::Record(record) => {
                        fmt!(f, "(");
                        let mut written = 0;
                        for (field, keep) in record.fields.iter().zip(keep.iter()) {
                            if !keep {
                                continue;
                            }
                            if written > 0 {
                                fmt!(f, ", ");
                            }
                            written += 1;
                            fmt!(f, field);
                        }
                        fmt!(f, ")");
                    }
                    other => fmt!(f, other),
                }
            }
        } else {
            let rows = Comma(self.rows.iter());
            fmt!(f, "VALUES " rows)
        }
    }
}
