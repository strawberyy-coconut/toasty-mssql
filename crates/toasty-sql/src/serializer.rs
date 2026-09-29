#[macro_use]
mod fmt;
use fmt::ToSql;

mod column;
use column::ColumnAlias;

mod cte;

mod delim;
use delim::{Comma, Delimited, Period};

mod dialect;

mod ident;
use ident::Ident;

mod params;
pub use params::Placeholder;

// Fragment serializers
mod column_def;
mod expr;
mod name;
mod statement;
mod ty;
mod value;

use crate::stmt::Statement;

use toasty_core::{
    driver::{
        Dialect,
        operation::{IsolationLevel, Transaction, TransactionMode},
    },
    schema::db::{self, Index, Table},
    stmt::IntoExprTarget,
};

/// Serialize a statement to a SQL string
#[derive(Debug)]
pub struct Serializer<'a> {
    /// Schema against which the statement is to be serialized
    schema: &'a db::Schema,

    /// The SQL dialect handles the differences between databases and
    /// supported features.
    dialect: Dialect,

    /// SQL emitted for [`TransactionMode::Default`] under the SQLite dialect.
    /// Constructors that don't override this leave it at `"BEGIN"`, which is
    /// SQLite's natural default (DEFERRED). A driver that wants `Default` to
    /// mean something engine-specific — Turso under `concurrent_writes()`
    /// uses `"BEGIN CONCURRENT"` — sets this through
    /// [`Self::sqlite_with_default_begin`]. The non-`Default` variants
    /// (`Deferred`, `Immediate`, `Exclusive`) always emit fixed SQL.
    sqlite_default_begin: &'static str,
}

struct Formatter<'a> {
    /// Handle to the serializer
    serializer: &'a Serializer<'a>,

    /// Expression-resolution context for the current scope. Re-scoped (via
    /// [`Formatter::scope`]) each time serialization descends into a new
    /// query level, so it travels with the formatter rather than as a
    /// separate argument.
    cx: ExprContext<'a>,

    /// Where to write the serialized SQL
    dst: &'a mut String,

    /// Current query depth. This is used to determine the nesting level when
    /// generating names
    depth: usize,

    /// True when table names should be aliased.
    alias: bool,

    /// True when inside an INSERT statement. Used by MySQL to decide whether
    /// VALUES rows need the ROW() wrapper (required in subqueries but not in
    /// INSERT).
    in_insert: bool,

    /// Target table whose stored columns must be qualified inside a PostgreSQL
    /// upsert assignment to distinguish them from `excluded` columns.
    assignment_table: Option<db::TableId>,

    /// Collects `Expr::Arg(n)` positions in the order they appear in the SQL.
    /// Used by MySQL (which uses positional `?` without indices) to reorder
    /// the params vec to match placeholder occurrence order. Borrowed so a
    /// scoped child formatter writes through to the root's vec.
    arg_positions: &'a mut Vec<usize>,

    /// Set while rendering the body of a query that asked for a row lock, so
    /// its `FROM` clause can carry the T-SQL table hint. T-SQL has no trailing
    /// `FOR UPDATE`, so the lock has to be emitted on the source instead, and
    /// it is consumed by the first source rendered. Ignored by other dialects.
    row_lock: bool,

    /// Set while rendering a `MERGE`. The statement has a source relation whose
    /// column names match the table being written, so columns of the written
    /// table are qualified to stay unambiguous. Only SQL Server reads this.
    merge: bool,

    /// Set while rendering an `OUTPUT` clause, so column references become
    /// `INSERTED.[col]` / `DELETED.[col]`. Only SQL Server reads this.
    output: Option<&'static str>,

    /// For an INSERT, one flag per target column marking whether the column is
    /// actually written. SQL Server drops generated (`IDENTITY`) columns: it
    /// rejects `DEFAULT` or `NULL` as an explicit identity value.
    insert_columns: Option<Vec<bool>>,
}

impl<'a> Formatter<'a> {
    /// Descend into a new expression scope, returning a child formatter that
    /// shares this one's output sink and arg collector (so writes flow back
    /// to the root) but resolves references against `target`.
    ///
    /// The child borrows `self`, so the parent scope stays live on the stack
    /// for the child's lifetime — that is what keeps the `ExprContext` parent
    /// chain valid for nested-reference resolution.
    fn scope<'c>(&'c mut self, target: impl IntoExprTarget<'c, db::Schema>) -> Formatter<'c> {
        Formatter {
            serializer: self.serializer,
            cx: self.cx.scope(target),
            dst: &mut *self.dst,
            depth: self.depth,
            alias: self.alias,
            in_insert: self.in_insert,
            assignment_table: self.assignment_table,
            arg_positions: &mut *self.arg_positions,
            row_lock: self.row_lock,
            merge: self.merge,
            output: self.output,
            insert_columns: self.insert_columns.clone(),
        }
    }
}

/// Expression context bound to a database-level schema.
pub type ExprContext<'a> = toasty_core::stmt::ExprContext<'a, db::Schema>;

impl<'a> Serializer<'a> {
    /// Serializes a [`Statement`] to a SQL string with all values inlined as
    /// literals (no bind parameters). Appends a trailing semicolon.
    ///
    /// Use this for DDL statements (`CREATE TABLE`, `CREATE TYPE`, etc.) where
    /// bind parameters are not supported. DML statements should already have
    /// their parameters extracted (as `Expr::Arg` placeholders) before reaching
    /// the serializer.
    pub fn serialize(&self, stmt: &Statement) -> String {
        self.serialize_with_arg_order(stmt).0
    }

    /// Serializes a [`Statement`] and returns both the SQL string and the order
    /// in which `Expr::Arg(n)` placeholders appear in the SQL.
    ///
    /// The arg order is needed by MySQL which uses positional `?` without
    /// indices — the caller must reorder its params vec to match the occurrence
    /// order. PostgreSQL and SQLite use indexed placeholders (`$1`, `?1`) so
    /// they can ignore the arg order.
    pub fn serialize_with_arg_order(&self, stmt: &Statement) -> (String, Vec<usize>) {
        let mut ret = String::new();
        let mut arg_positions = Vec::new();

        {
            let mut fmt = Formatter {
                serializer: self,
                cx: ExprContext::new(self.schema),
                dst: &mut ret,
                depth: 0,
                alias: false,
                in_insert: false,
                assignment_table: None,
                arg_positions: &mut arg_positions,
                row_lock: false,
                merge: false,
                output: None,
                insert_columns: None,
            };

            stmt.to_sql(&mut fmt);
        }

        ret.push(';');
        (ret, arg_positions)
    }

    /// Serialize a transaction control operation to a SQL string.
    ///
    /// The generated SQL is dialect-specific (e.g., MySQL uses `START TRANSACTION`
    /// while other databases use `BEGIN`). Savepoints are named `sp_{id}`.
    pub fn serialize_transaction(&self, op: &Transaction) -> String {
        let mut ret = String::new();
        let mut arg_positions = Vec::new();

        {
            let mut f = Formatter {
                serializer: self,
                cx: ExprContext::new(self.schema),
                dst: &mut ret,
                depth: 0,
                alias: false,
                in_insert: false,
                assignment_table: None,
                arg_positions: &mut arg_positions,
                row_lock: false,
                merge: false,
                output: None,
                insert_columns: None,
            };

            match op {
                Transaction::Start {
                    isolation,
                    read_only,
                    mode,
                } => fmt!(
                    &mut f,
                    self.serialize_transaction_start(*isolation, *read_only, *mode)
                ),
                Transaction::Commit => fmt!(&mut f, "COMMIT"),
                Transaction::Rollback => fmt!(&mut f, "ROLLBACK"),
                Transaction::Savepoint(name) if matches!(f.serializer.dialect, Dialect::Mssql) => {
                    fmt!(&mut f, "SAVE TRANSACTION " Ident(name))
                }
                Transaction::ReleaseSavepoint(name)
                    if matches!(f.serializer.dialect, Dialect::Mssql) =>
                {
                    // T-SQL has no `RELEASE SAVEPOINT`: a savepoint is discarded
                    // when the transaction commits. The driver treats this as a
                    // no-op, so the placeholder keeps the statement well formed.
                    fmt!(&mut f, "SELECT 1 WHERE 1 = 0 /* RELEASE SAVEPOINT " Ident(name) " */")
                }
                Transaction::RollbackToSavepoint(name)
                    if matches!(f.serializer.dialect, Dialect::Mssql) =>
                {
                    fmt!(&mut f, "ROLLBACK TRANSACTION " Ident(name))
                }
                Transaction::Savepoint(name) => {
                    fmt!(&mut f, "SAVEPOINT " Ident(name))
                }
                Transaction::ReleaseSavepoint(name) => {
                    fmt!(&mut f, "RELEASE SAVEPOINT " Ident(name))
                }
                Transaction::RollbackToSavepoint(name) => {
                    fmt!(&mut f, "ROLLBACK TO SAVEPOINT " Ident(name))
                }
            };
        }

        ret.push(';');
        ret
    }

    fn serialize_transaction_start(
        &self,
        isolation: Option<IsolationLevel>,
        read_only: bool,
        mode: TransactionMode,
    ) -> String {
        fn isolation_level_str(level: IsolationLevel) -> &'static str {
            match level {
                IsolationLevel::ReadUncommitted => "READ UNCOMMITTED",
                IsolationLevel::ReadCommitted => "READ COMMITTED",
                IsolationLevel::RepeatableRead => "REPEATABLE READ",
                IsolationLevel::Serializable => "SERIALIZABLE",
            }
        }

        match self.dialect {
            // MySQL has no SQLite-style lock-mode keyword; drivers
            // reject non-Default `mode` before reaching the serializer.
            Dialect::Mysql | Dialect::MariaDb => {
                let mut sql = String::new();
                if let Some(level) = isolation {
                    sql.push_str("SET TRANSACTION ISOLATION LEVEL ");
                    sql.push_str(isolation_level_str(level));
                    sql.push_str("; ");
                }
                sql.push_str("START TRANSACTION");
                if read_only {
                    sql.push_str(" READ ONLY");
                }
                sql
            }
            // PostgreSQL has no SQLite-style lock-mode keyword; drivers
            // reject non-Default `mode` before reaching the serializer.
            Dialect::Postgresql => {
                let mut sql = String::from("BEGIN");
                if let Some(level) = isolation {
                    sql.push_str(" ISOLATION LEVEL ");
                    sql.push_str(isolation_level_str(level));
                }
                if read_only {
                    sql.push_str(" READ ONLY");
                }
                sql
            }
            // SQL Server has no SQLite-style lock-mode keyword; drivers reject
            // non-`Default` `mode` before reaching the serializer. The isolation
            // level is set for the next transaction, then the transaction starts.
            Dialect::Mssql => {
                let mut sql = String::new();
                if let Some(level) = isolation {
                    sql.push_str("SET TRANSACTION ISOLATION LEVEL ");
                    sql.push_str(isolation_level_str(level));
                    sql.push_str("; ");
                }
                sql.push_str("BEGIN TRANSACTION");
                sql
            }
            // SQLite has no per-transaction isolation level or read-only
            // keyword; the lock-acquisition mode is the only knob. `Default`
            // emits whatever the serializer was configured with at
            // construction (`BEGIN` by default, or e.g. `BEGIN CONCURRENT`
            // for Turso under MVCC). `Deferred`/`Immediate`/`Exclusive` are
            // explicit caller requests with fixed SQL.
            Dialect::Sqlite => match mode {
                TransactionMode::Default => self.sqlite_default_begin.to_string(),
                TransactionMode::Deferred => "BEGIN".to_string(),
                TransactionMode::Immediate => "BEGIN IMMEDIATE".to_string(),
                TransactionMode::Exclusive => "BEGIN EXCLUSIVE".to_string(),
            },
        }
    }

    fn table(&self, id: impl Into<db::TableId>) -> &'a Table {
        self.schema.table(id.into())
    }

    fn index(&self, id: impl Into<db::IndexId>) -> &'a Index {
        self.schema.index(id.into())
    }

    fn table_name(&self, id: impl Into<db::TableId>) -> Ident<&str> {
        let table = self.schema.table(id.into());
        Ident(&table.name)
    }

    fn column_name(&self, id: impl Into<db::ColumnId>) -> Ident<&str> {
        let column = self.schema.column(id.into());
        Ident(&column.name)
    }
}

// ---------------------------------------------------------------------------
// SQL Server carried function calls
//
// Toasty's expression AST has no variant for "call a function by name", and it
// is a closed enum, so a SQL Server driver cannot add one. A call is therefore
// carried in the node Toasty uses for `#[document]` paths,
// `stmt::FuncJsonExtract`, which holds both an arbitrary operand expression
// (`base`) and a `Vec<String>` (`path`):
//
//     FuncJsonExtract {
//         base: <the column expression>,
//         path: ["!ISJSON", "{base}", "ARRAY"],   // name, then the arguments
//         ty:   <the result type>,
//     }
//
// The markers below distinguish a call from a real document path extraction;
// the SQL Server driver's model-facing function traits use them to encode a
// call, and the expression renderer decodes it here. They live beside the
// renderer so the wire format has a single definition.

/// Marks a `path` as a carried call rather than a document path extraction.
#[doc(hidden)]
pub const MSSQL_FUNC_MARKER: char = '!';

/// Introduces a carried call that is a method on the operand: `col.STArea()`.
#[doc(hidden)]
pub const MSSQL_FUNC_METHOD: char = '.';

/// Stands where the operand goes in an argument list.
///
/// Not every T-SQL function takes its subject first: `DATEPART(day, col)` and
/// `CHARINDEX(N'needle', col)` both take it last, so the argument list carries
/// the position rather than assuming it.
#[doc(hidden)]
pub const MSSQL_FUNC_OPERAND: &str = "{base}";

/// A decoded SQL Server carried function call.
pub(crate) struct MssqlFuncCall<'a> {
    /// The function or method name, with any prefix already stripped.
    pub(crate) name: &'a str,
    /// Already-escaped T-SQL text for each argument.
    pub(crate) args: &'a [String],
    /// Whether the operand is the receiver (`col.STArea()`) rather than an
    /// argument (`ISJSON(col)`).
    pub(crate) method: bool,
}

/// Returns the call encoded in `path`, or `None` when this is a real document
/// path extraction.
pub(crate) fn mssql_decode_func_call(path: &[String]) -> Option<MssqlFuncCall<'_>> {
    let (head, args) = path.split_first()?;
    let name = head.strip_prefix(MSSQL_FUNC_MARKER)?;

    let (method, name) = match name.strip_prefix(MSSQL_FUNC_METHOD) {
        Some(name) => (true, name),
        None => (false, name),
    };

    Some(MssqlFuncCall { name, args, method })
}

/// Whether `arg` is the placeholder for the operand rather than a literal.
pub(crate) fn mssql_func_is_operand(arg: &str) -> bool {
    arg == MSSQL_FUNC_OPERAND
}
