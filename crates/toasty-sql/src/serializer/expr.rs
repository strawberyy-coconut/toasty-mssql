use toasty_core::{schema::db::ColumnId, stmt::ResolvedRef};

use super::{ColumnAlias, Comma, Delimited, Ident, ToSql};

use crate::{serializer::Dialect, stmt};

/// The collation that makes a comparison case-sensitive on SQL Server.
///
/// SQL Server's default collation is case-insensitive, which is not what
/// Toasty's `starts_with` and collection-membership predicates promise, so the
/// comparisons that need it opt in to a binary collation.
const CASE_SENSITIVE_COLLATION: &str = "Latin1_General_BIN2";

impl ToSql for &stmt::Expr {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match self {
            stmt::Expr::And(expr) => {
                if f.serializer.is_mssql() {
                    fmt!(
                        f,
                        Delimited(expr.operands.iter().map(MssqlPredicate), " AND ")
                    );
                } else {
                    fmt!(f, Delimited(expr.operands.iter().map(AndOperand), " AND "));
                }
            }
            stmt::Expr::Between(expr) => {
                fmt!(f, expr.expr " BETWEEN " expr.low " AND " expr.high);
            }
            stmt::Expr::BinaryOp(expr) => {
                assert!(!expr.lhs.is_value_null());
                assert!(!expr.rhs.is_value_null());

                fmt!(f, expr.lhs " " expr.op " " expr.rhs);
            }
            stmt::Expr::Exists(expr) => {
                f.depth += 1;
                fmt!(f, "EXISTS (" expr.subquery ")");
                f.depth -= 1;
            }
            stmt::Expr::Func(stmt::ExprFunc::Count(func)) => match (&func.arg, &func.filter) {
                (None, None) => fmt!(f, "COUNT(*)"),
                // MySQL does not support filters, and neither does T-SQL, so
                // translate the filter into a conditional aggregate.
                (None, Some(expr)) if f.serializer.is_mysql() || f.serializer.is_mssql() => {
                    fmt!(f, "COUNT(CASE WHEN " expr " THEN 1 END)")
                }
                (None, Some(expr)) => fmt!(f, "COUNT(*) FILTER (WHERE " expr ")"),
                _ => todo!("func={func:#?}"),
            },
            stmt::Expr::Func(stmt::ExprFunc::LastInsertId(_)) => {
                if f.serializer.is_mssql() {
                    // `@@IDENTITY` crosses triggers; `SCOPE_IDENTITY` is the
                    // current scope's last identity, which is what an insert
                    // returning its generated key wants.
                    fmt!(f, "SCOPE_IDENTITY()")
                } else {
                    fmt!(f, "LAST_INSERT_ID()")
                }
            }
            stmt::Expr::Func(stmt::ExprFunc::JsonExtract(func)) => {
                serialize_json_extract(f, func);
            }
            stmt::Expr::Project(project)
                if let stmt::Expr::Incoming(incoming) = project.base.as_ref() =>
            {
                let stmt::ExprIncoming::Table(table) = incoming else {
                    panic!("incoming projection was not lowered")
                };
                let [column] = project.projection.as_slice() else {
                    panic!("lowered incoming projection must reference one column")
                };
                let column = ColumnId {
                    table: *table,
                    index: *column,
                };
                // SQL Server names the row an upsert proposes through the
                // `MERGE` source relation rather than `excluded`.
                if f.serializer.is_mssql() {
                    fmt!(f, "src." f.serializer.column_name(column));
                } else {
                    fmt!(f, "excluded." f.serializer.column_name(column));
                }
            }
            stmt::Expr::Incoming(_) => panic!("incoming row must be projected"),
            stmt::Expr::IsSuperset(e) => match f.serializer.dialect {
                Dialect::Postgresql => fmt!(f, e.lhs.as_ref() " @> " e.rhs.as_ref()),
                // The rhs Value::List is bound as one JSON string. MySQL's
                // `JSON_CONTAINS(target, candidate)` matches when every
                // element of `candidate` appears in `target`.
                Dialect::Mysql | Dialect::MariaDb => {
                    fmt!(f, "JSON_CONTAINS(" e.lhs.as_ref() ", " e.rhs.as_ref() ")")
                }
                // SQLite has no direct superset operator; emulate via
                // `NOT EXISTS (rhs element with no match in lhs)`.
                Dialect::Sqlite => fmt!(
                    f,
                    "NOT EXISTS (SELECT 1 FROM json_each(" e.rhs.as_ref()
                    ") AS r WHERE r.value NOT IN (SELECT l.value FROM json_each("
                    e.lhs.as_ref() ") AS l))"
                ),
                // T-SQL has no set operator over a JSON array. The capability
                // reports `native_array_set_predicates: false`, so the engine
                // rewrites this into one membership test per rhs element before
                // the driver sees it.
                Dialect::Mssql => unreachable!(
                    "SQL Server has no native array set predicates; the engine must rewrite IsSuperset; expr={e:?}"
                ),
            },
            stmt::Expr::Intersects(e) => match f.serializer.dialect {
                Dialect::Postgresql => fmt!(f, e.lhs.as_ref() " && " e.rhs.as_ref()),
                Dialect::Mysql | Dialect::MariaDb => {
                    fmt!(f, "JSON_OVERLAPS(" e.lhs.as_ref() ", " e.rhs.as_ref() ")")
                }
                Dialect::Sqlite => fmt!(
                    f,
                    "EXISTS (SELECT 1 FROM json_each(" e.rhs.as_ref()
                    ") AS r WHERE r.value IN (SELECT l.value FROM json_each("
                    e.lhs.as_ref() ") AS l))"
                ),
                // As with `IsSuperset`, the engine rewrites this because the
                // capability reports no native set predicate.
                Dialect::Mssql => unreachable!(
                    "SQL Server has no native array set predicates; the engine must rewrite Intersects; expr={e:?}"
                ),
            },
            stmt::Expr::Length(e) => match f.serializer.dialect {
                Dialect::Postgresql => fmt!(f, "cardinality(" e.expr.as_ref() ")"),
                Dialect::Mysql | Dialect::MariaDb => fmt!(f, "JSON_LENGTH(" e.expr.as_ref() ")"),
                Dialect::Sqlite => fmt!(f, "json_array_length(" e.expr.as_ref() ")"),
                // `OPENJSON` is the only way to enumerate a JSON array in T-SQL
                // before SQL Server 2025; its row count is the element count.
                Dialect::Mssql => fmt!(
                    f,
                    "(SELECT COUNT(*) FROM OPENJSON(" e.expr.as_ref() "))"
                ),
            },
            stmt::Expr::Ident(name) => {
                fmt!(f, Ident(name));
            }
            stmt::Expr::InList(expr) => {
                // A composite key compares a row value, which T-SQL does not
                // have, so `(a, b) IN ((x, y), (z, w))` has to be expanded into
                // an `OR` of `AND`s.
                if f.serializer.is_mssql()
                    && let Some(fields) = row_fields(&expr.expr)
                {
                    serialize_mssql_row_in_list(f, fields, &expr.list);
                    return;
                }

                fmt!(f, expr.expr " IN " expr.list);
            }
            stmt::Expr::AnyOp(expr) => match f.serializer.dialect {
                // `value = ANY(col)` — PostgreSQL's array membership operator.
                // Drives `Path::contains` for native-array columns and the
                // IN-list rewrite.
                Dialect::Postgresql => {
                    fmt!(f, expr.lhs " " expr.op " ANY(" expr.rhs ")");
                }
                // MySQL's `value MEMBER OF (json_array)` (8.0.17+). Only the
                // equality form makes sense; `Path::contains` is the only
                // current emitter and the lowering pass never produces
                // ANY on MySQL since `predicate_match_any` is false.
                Dialect::Mysql if matches!(expr.op, stmt::BinaryOp::Eq) => {
                    fmt!(f, expr.lhs " MEMBER OF (" expr.rhs ")");
                }
                Dialect::Mysql => unreachable!("AnyOp with non-Eq operator on MySQL: {expr:?}"),
                // MariaDB lacks MEMBER OF. JSON_ARRAY preserves the scalar's
                // JSON type and makes string membership case-sensitive.
                Dialect::MariaDb if matches!(expr.op, stmt::BinaryOp::Eq) => {
                    fmt!(f, "JSON_CONTAINS(" expr.rhs ", JSON_ARRAY(" expr.lhs "))");
                }
                Dialect::MariaDb => {
                    unreachable!("AnyOp with non-Eq operator on MariaDB: {expr:?}")
                }
                // SQLite renders `value = ANY(col)` (i.e. `Path::contains`)
                // as `value IN (SELECT value FROM json_each(col))`.
                Dialect::Sqlite if matches!(expr.op, stmt::BinaryOp::Eq) => {
                    fmt!(
                        f,
                        expr.lhs " IN (SELECT value FROM json_each(" expr.rhs "))"
                    );
                }
                Dialect::Sqlite => {
                    unreachable!("AnyOp with non-Eq operator on SQLite: {expr:?}")
                }
                // SQL Server has no array type, and its `ANY` quantifier only
                // accepts a subquery, so the collection is enumerated with
                // `OPENJSON`. The elements are forced to a binary collation
                // because membership must be case-sensitive while the server
                // default collation is not.
                Dialect::Mssql if matches!(expr.op, stmt::BinaryOp::Eq) => {
                    fmt!(
                        f,
                        expr.lhs " IN (SELECT [value] COLLATE " CASE_SENSITIVE_COLLATION
                        " FROM OPENJSON(" expr.rhs "))"
                    );
                }
                Dialect::Mssql => {
                    unreachable!("AnyOp with non-Eq operator on SQL Server: {expr:?}")
                }
            },
            stmt::Expr::AllOp(expr) => {
                fmt!(f, expr.lhs " " expr.op " ALL(" expr.rhs ")");
            }
            stmt::Expr::InSubquery(expr) => {
                // A row value cannot be compared against a subquery in T-SQL
                // either, so a composite key wraps the subquery in a derived
                // table with positional column names.
                if f.serializer.is_mssql()
                    && let Some(fields) = row_fields(&expr.expr)
                {
                    serialize_mssql_row_in_subquery(f, fields, &expr.query, expr.negated);
                    return;
                }

                let op = if expr.negated { " NOT IN (" } else { " IN (" };
                fmt!(f, expr.expr op expr.query ")");
            }
            stmt::Expr::IsNull(expr) => {
                let op = if expr.negated {
                    " IS NOT NULL"
                } else {
                    " IS NULL"
                };
                fmt!(f, expr.expr op);
            }
            stmt::Expr::Like(expr) => {
                let op = if expr.case_insensitive
                    && matches!(f.serializer.dialect, Dialect::Postgresql)
                {
                    " ILIKE "
                } else {
                    " LIKE "
                };
                fmt!(f, expr.expr op expr.pattern);
                if let Some(escape) = expr.escape {
                    let escape = if f.serializer.is_mysql() && escape == '\\' {
                        stmt::Value::String("\\\\".to_string())
                    } else {
                        stmt::Value::String(escape.to_string())
                    };
                    let escape = &escape;
                    fmt!(f, " ESCAPE " escape);
                }
            }
            stmt::Expr::StartsWith(expr) => {
                match f.serializer.dialect {
                    // PostgreSQL's `^@` prefix-match operator; prefix is bound
                    // as a plain string parameter.
                    Dialect::Postgresql => {
                        fmt!(f, expr.expr " ^@ " expr.prefix);
                    }
                    // SQLite GLOB is case-sensitive.  extract_params has already
                    // escaped GLOB metacharacters and appended `*` to the prefix
                    // parameter, so we only need to emit the right operator.
                    Dialect::Sqlite => {
                        fmt!(f, expr.expr " GLOB " expr.prefix);
                    }
                    // MySQL LIKE is case-insensitive by default; casting the
                    // column side to BINARY forces a case-sensitive byte
                    // comparison.  extract_params has escaped `%`/`_`/`!` and
                    // appended `%` to the prefix parameter.
                    Dialect::Mysql | Dialect::MariaDb => {
                        fmt!(f, "BINARY " expr.expr " LIKE " expr.prefix " ESCAPE '!'");
                    }
                    // SQL Server has no dedicated prefix operator. The planner
                    // rewrote the prefix into a finished `LIKE` pattern with `%`,
                    // `_` and `!` escaped and a trailing `%` appended, so this
                    // only has to force a case-sensitive comparison with a
                    // binary collation and name the escape character. The
                    // operand is parenthesised because `COLLATE` binds to
                    // whatever expression precedes it.
                    Dialect::Mssql => {
                        fmt!(
                            f,
                            "((" expr.expr ") COLLATE " CASE_SENSITIVE_COLLATION
                            " LIKE " expr.prefix " ESCAPE '!')"
                        );
                    }
                }
            }
            stmt::Expr::Not(expr) => {
                // T-SQL has no boolean expression type, so the operand of `NOT`
                // must itself be a predicate.
                if f.serializer.is_mssql() {
                    fmt!(f, "NOT (" MssqlPredicate(expr.expr.as_ref()) ")");
                } else {
                    fmt!(f, "NOT (" expr.expr ")");
                }
            }
            stmt::Expr::Or(expr) => {
                if f.serializer.is_mssql() {
                    fmt!(
                        f,
                        Delimited(expr.operands.iter().map(MssqlPredicate), " OR ")
                    );
                } else {
                    fmt!(f, Delimited(&expr.operands, " OR "));
                }
            }
            stmt::Expr::Record(expr) => {
                let fields = Comma(expr.fields.iter());
                fmt!(f, "(" fields ")");
            }
            stmt::Expr::Reference(expr_reference @ stmt::ExprReference::Column(expr_column)) => {
                if f.alias {
                    let depth = f.depth - expr_column.nesting;

                    match f.cx.resolve_expr_reference(expr_reference) {
                        ResolvedRef::Column(column) => {
                            let name = Ident(&column.name);
                            fmt!(f, "tbl_" depth "_" expr_column.table "." name)
                        }
                        ResolvedRef::Cte { .. } | ResolvedRef::Derived(_) => {
                            fmt!(f, "tbl_" depth "_" expr_column.table "." ColumnAlias(expr_column.column))
                        }
                        ResolvedRef::Model(model) => {
                            panic!("Model references cannot be serialized to SQL; model={model:?}")
                        }
                        ResolvedRef::Field(field) => {
                            panic!("Field references cannot be serialized to SQL; field={field:?}")
                        }
                    }
                } else {
                    let column =
                        f.cx.resolve_expr_reference(expr_reference)
                            .as_column_unwrap();
                    // Inside an `OUTPUT` clause a column names the affected side
                    // of the write, not the merged table.
                    if let Some(prefix) = f.output {
                        fmt!(f, prefix "." Ident(&column.name))
                    } else if f.merge {
                        // A `MERGE`'s source relation shares its column names
                        // with the written table, so the stored column is
                        // qualified.
                        fmt!(f, "target." Ident(&column.name))
                    } else if matches!(f.serializer.dialect, Dialect::Postgresql)
                        && expr_column.nesting == 0
                        && f.assignment_table == Some(column.id.table)
                    {
                        fmt!(f, f.serializer.table_name(column.id.table) "." Ident(&column.name))
                    } else {
                        fmt!(f, Ident(&column.name))
                    }
                }
            }
            stmt::Expr::Stmt(expr) => {
                let stmt = &*expr.stmt;
                fmt!(f, "(" stmt ")");
            }
            stmt::Expr::List(expr) => {
                let items = Comma(expr.items.iter());
                fmt!(f, "(" items ")");
            }
            stmt::Expr::Value(expr) => expr.to_sql(f),
            // Schema-fixed leaf rendered inline as a SQL literal.  Reuses
            // the same `Value::to_sql` path the inline DDL serializer uses,
            // which escapes `String` defensively.
            stmt::Expr::Static(expr) => expr.to_sql(f),
            stmt::Expr::Arg(arg) => {
                // Pre-extracted bind parameter placeholder — render as a
                // positional parameter. The arg position is 0-based; the
                // placeholder is 1-based.
                f.arg_positions.push(arg.position);
                let placeholder = super::Placeholder(arg.position + 1);
                fmt!(f, placeholder);
            }
            stmt::Expr::Default => match f.serializer.dialect {
                Dialect::Postgresql | Dialect::Mysql | Dialect::MariaDb | Dialect::Mssql => {
                    fmt!(f, "DEFAULT")
                }
                // SQLite does not support the DEFAULT keyword but NULL acts similarly.
                Dialect::Sqlite => fmt!(f, "NULL"),
            },
            _ => todo!("expr={:#?}", self),
        }
    }
}

/// Whether an expression is usable as a T-SQL predicate without a comparison.
///
/// T-SQL has no boolean expression type: a condition has to be a comparison, a
/// quantified predicate, `IS NULL`, or a logical combination of those. Anything
/// else in a condition position — a boolean column reference, or a boolean
/// literal the folding pass produced — must be compared to `1`.
pub(super) fn mssql_is_predicate(expr: &stmt::Expr) -> bool {
    match expr {
        stmt::Expr::BinaryOp(op) => matches!(
            op.op,
            stmt::BinaryOp::Eq
                | stmt::BinaryOp::Ne
                | stmt::BinaryOp::Ge
                | stmt::BinaryOp::Gt
                | stmt::BinaryOp::Le
                | stmt::BinaryOp::Lt
        ),
        stmt::Expr::And(_)
        | stmt::Expr::Or(_)
        | stmt::Expr::Not(_)
        | stmt::Expr::IsNull(_)
        | stmt::Expr::Between(_)
        | stmt::Expr::Like(_)
        | stmt::Expr::StartsWith(_)
        | stmt::Expr::InList(_)
        | stmt::Expr::InSubquery(_)
        | stmt::Expr::Exists(_)
        | stmt::Expr::AnyOp(_)
        | stmt::Expr::AllOp(_)
        | stmt::Expr::Intersects(_)
        | stmt::Expr::IsSuperset(_) => true,
        _ => false,
    }
}

/// Renders an expression where T-SQL expects a condition.
///
/// A non-predicate operand — a `BIT` value or column, which T-SQL cannot use as
/// a bare condition — is compared to `1`.
pub(super) struct MssqlPredicate<'a>(pub(super) &'a stmt::Expr);

impl ToSql for MssqlPredicate<'_> {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        if mssql_is_predicate(self.0) {
            fmt!(f, self.0);
        } else {
            fmt!(f, "(" self.0 " = 1)");
        }
    }
}

/// A single operand of an `AND` chain.
///
/// `OR` binds looser than `AND` in SQL, so an `Or` operand must be
/// parenthesized: `a AND (b OR c)` would otherwise serialize as
/// `a AND b OR c`, which parses as `(a AND b) OR c` and silently changes the
/// query's meaning. Operands of other kinds bind at least as tightly as `AND`
/// (comparisons, `IS NULL`, `NOT (..)`, nested `AND`), so they need no parens.
struct AndOperand<'a>(&'a stmt::Expr);

impl ToSql for AndOperand<'_> {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        if matches!(self.0, stmt::Expr::Or(_)) {
            fmt!(f, "(" self.0 ")");
        } else {
            fmt!(f, self.0);
        }
    }
}

/// Serializes a document path extraction per dialect: `json_extract(col,
/// '$.a.b')` on SQLite, `CAST(JSON_UNQUOTE(JSON_EXTRACT(col, '$.a.b')) AS ...)`
/// on MySQL, and `(col->'a'->>'b')::cast` on PostgreSQL — the latter two unwrap
/// the leaf to text and cast it to match the bound parameter's type.
fn serialize_json_extract(f: &mut super::Formatter<'_>, func: &stmt::FuncJsonExtract) {
    match f.serializer.dialect {
        Dialect::Sqlite => {
            // SQLite's `json_extract` returns SQL-native scalars (unquoted text,
            // integers, reals), so a path read compares directly against a bound
            // parameter with no cast. The path is a single-quoted JSONPath like
            // `$.a.b`.
            fmt!(
                f,
                "json_extract(" func.base.as_ref() ", '$"
                Delimited(func.path.iter().map(|key| (".", key.as_str())), "")
                "')"
            );
        }
        Dialect::Mysql | Dialect::MariaDb => serialize_mysql_json_extract(f, func),
        Dialect::Mssql => serialize_mssql_json_extract(f, func),
        Dialect::Postgresql => {
            // Descend with `->`, take the leaf as text with `->>`, then cast the
            // text to the leaf type so it compares against a bound parameter.
            let (leaf, parents) = func
                .path
                .split_last()
                .expect("json extract path has at least one key");
            fmt!(
                f,
                "(" func.base.as_ref()
                Delimited(parents.iter().map(|key| ("->'", key.as_str(), "'")), "")
                "->>'" leaf.as_str() "')"
                pg_json_cast(&func.ty).map(|cast| ("::", cast))
            );
        }
    }
}

/// Serializes a MySQL document path read. `JSON_EXTRACT` yields a *JSON-typed*
/// value, which compares against a bound SQL parameter only by luck — a JSON
/// string (e.g. an ISO timestamp) never equals a native `DATETIME`, and JSON
/// string comparison is `utf8mb4_bin` (case-sensitive), unlike a `VARCHAR`
/// column. So unwrap the leaf to text with `JSON_UNQUOTE` and `CAST` it to the
/// leaf's SQL type (`CHAR` for strings, to recover a `VARCHAR`-matching
/// collation — see [`mysql_json_cast`]), mirroring the `->>`-plus-cast
/// PostgreSQL path. Booleans are the exception: the unquoted text
/// `'true'`/`'false'` casts to `0`, so cast the *bare* JSON boolean to
/// `UNSIGNED` instead (`true` -> 1, `false` -> 0), matching a bound bool param.
fn serialize_mysql_json_extract(f: &mut super::Formatter<'_>, func: &stmt::FuncJsonExtract) {
    if matches!(func.ty, stmt::Type::Bool) {
        fmt!(f, "CAST(");
        mysql_json_extract(f, func);
        fmt!(f, " AS UNSIGNED)");
    } else if let Some(cast) = mysql_json_cast(&func.ty) {
        fmt!(f, "CAST(JSON_UNQUOTE(");
        mysql_json_extract(f, func);
        fmt!(f, ") AS " cast ")");
    } else {
        fmt!(f, "JSON_UNQUOTE(");
        mysql_json_extract(f, func);
        fmt!(f, ")");
    }
}

/// Emits the bare `JSON_EXTRACT(col, '$.a.b')` every MySQL path read is built
/// on, with the path as a single-quoted JSONPath argument.
fn mysql_json_extract(f: &mut super::Formatter<'_>, func: &stmt::FuncJsonExtract) {
    fmt!(
        f,
        "JSON_EXTRACT(" func.base.as_ref() ", '$"
        Delimited(func.path.iter().map(|key| (".", key.as_str())), "")
        "')"
    );
}

/// The MySQL `CAST(... AS <type>)` target wrapped around a
/// `JSON_UNQUOTE(JSON_EXTRACT(...))` text extraction so it compares against a
/// bound parameter of the leaf type. Mirrors [`pg_json_cast`]; `None` leaves the
/// extraction as bare unquoted text (only floats, which compare via numeric
/// coercion). `Bool` is absent because it is cast separately — see
/// [`serialize_mysql_json_extract`].
///
/// `String`/`Uuid` cast to `CHAR` not for the type but for the *collation*:
/// `JSON_UNQUOTE` yields `utf8mb4_bin` (case-sensitive), while `CAST(... AS
/// CHAR)` adopts the connection's default collation — the same one a bound
/// literal and a `VARCHAR` column use — so a string filter on a document leaf
/// matches the case sensitivity of a plain column. `AS CHAR` (no length) does
/// not truncate, and inheriting the server default keeps it portable across
/// server collation configs rather than hardcoding a collation name.
///
/// The temporal targets carry `(6)` precision: a bare `CAST(... AS DATETIME)`
/// truncates to whole seconds, which would drop the microseconds the JSON codec
/// writes and break an equality filter on a sub-second value.
fn mysql_json_cast(ty: &stmt::Type) -> Option<&'static str> {
    use crate::stmt::Type;
    Some(match ty {
        Type::String | Type::Uuid => "CHAR",
        Type::I8 | Type::I16 | Type::I32 | Type::I64 => "SIGNED",
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => "UNSIGNED",
        #[cfg(feature = "rust_decimal")]
        Type::Decimal => "DECIMAL(65, 30)",
        #[cfg(feature = "bigdecimal")]
        Type::BigDecimal => "DECIMAL(65, 30)",
        #[cfg(feature = "jiff")]
        Type::Timestamp => "DATETIME(6)",
        #[cfg(feature = "jiff")]
        Type::Date => "DATE",
        #[cfg(feature = "jiff")]
        Type::Time => "TIME(6)",
        #[cfg(feature = "jiff")]
        Type::DateTime => "DATETIME(6)",
        _ => return None,
    })
}

/// The PostgreSQL cast applied to a `->>'` text extraction so it compares
/// against a bound parameter of the leaf type. `String` (and any non-scalar
/// leaf) needs no cast — `->>` already yields text.
///
/// Every scalar a `#[document]` leaf can hold must appear here: an unlisted
/// scalar falls through to `None` and renders as an *uncast* text extraction,
/// which PostgreSQL then refuses to compare against a typed parameter
/// (`operator does not exist: text = ...`). The temporal casts pair with the
/// microsecond-truncated text the JSON codec writes (see `toasty_sql::json`),
/// so the extracted value parses cleanly into the SQL temporal type. Network
/// addresses likewise cast from their canonical text forms. `Zoned` is
/// intentionally absent: it is rejected at schema-build because jiff renders
/// it with an RFC 9557 `[IANA]` annotation that no PostgreSQL cast can parse.
fn pg_json_cast(ty: &stmt::Type) -> Option<&'static str> {
    use crate::stmt::Type;
    Some(match ty {
        Type::Bool => "boolean",
        Type::I8 | Type::I16 | Type::I32 | Type::I64 => "bigint",
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => "bigint",
        Type::F32 | Type::F64 => "double precision",
        Type::Uuid => "uuid",
        #[cfg(feature = "rust_decimal")]
        Type::Decimal => "numeric",
        #[cfg(feature = "bigdecimal")]
        Type::BigDecimal => "numeric",
        #[cfg(feature = "jiff")]
        Type::Timestamp => "timestamptz",
        #[cfg(feature = "jiff")]
        Type::Date => "date",
        #[cfg(feature = "jiff")]
        Type::Time => "time",
        #[cfg(feature = "jiff")]
        Type::DateTime => "timestamp",
        #[cfg(feature = "net")]
        Type::Cidr => "cidr",
        #[cfg(feature = "net")]
        Type::Inet => "inet",
        #[cfg(feature = "net")]
        Type::MacAddr => "macaddr",
        #[cfg(feature = "net")]
        Type::MacAddr8 => "macaddr8",
        _ => return None,
    })
}

/// Renders a SQL Server document path read, or a carried scalar function call.
///
/// `JSON_VALUE` returns a scalar as `nvarchar(4000)` whatever it actually
/// holds, so the leaf's own type is cast back on: a number has to compare as a
/// number and a timestamp as a timestamp, not as the text that represents them.
/// `JSON_QUERY` is the counterpart for a leaf that is itself an object or
/// array, which `JSON_VALUE` would answer with `NULL`.
///
/// No collation is forced, deliberately: the suite requires a document string
/// leaf to match with the *same* case sensitivity as a plain column, and
/// `JSON_VALUE` inherits the input's collation, which already agrees with the
/// server-default column comparisons.
fn serialize_mssql_json_extract(f: &mut super::Formatter<'_>, func: &stmt::FuncJsonExtract) {
    // A carried scalar function call lives in the same node as a document path
    // read; the `!` marker is what separates them.
    if let Some(call) = super::mssql_decode_func_call(&func.path) {
        if call.method {
            // A method hangs off the operand — `col.STArea()` — because that is
            // the only form T-SQL offers for it.
            func.base.as_ref().to_sql(f);
            f.dst.push('.');
            f.dst.push_str(call.name);
            f.dst.push('(');
        } else {
            f.dst.push_str(call.name);
            f.dst.push('(');
        }

        for (index, arg) in call.args.iter().enumerate() {
            if index > 0 {
                f.dst.push_str(", ");
            }
            if super::mssql_func_is_operand(arg) {
                func.base.as_ref().to_sql(f);
            } else {
                f.dst.push_str(arg);
            }
        }

        f.dst.push(')');
        return;
    }

    let object = matches!(func.ty, stmt::Type::Object);
    f.dst.push_str(if object {
        "JSON_QUERY("
    } else {
        "CAST(JSON_VALUE("
    });
    func.base.as_ref().to_sql(f);
    f.dst.push_str(", '$");
    for key in &func.path {
        // Keys come from Rust field names, so they need no quoting.
        f.dst.push('.');
        f.dst.push_str(key);
    }
    f.dst.push_str("')");

    if object {
        return;
    }

    f.dst.push_str(" AS ");
    f.dst
        .push_str(mssql_cast_type(&func.ty).unwrap_or("NVARCHAR(MAX)"));
    f.dst.push(')');
}

/// The T-SQL `CAST(... AS <type>)` target for a scalar a document leaf can
/// hold, mirroring the storage types the capability declares.
fn mssql_cast_type(ty: &stmt::Type) -> Option<&'static str> {
    use crate::stmt::Type;

    Some(match ty {
        Type::Bool => "BIT",
        Type::I8 | Type::I16 => "SMALLINT",
        Type::I32 => "INT",
        Type::I64 => "BIGINT",
        Type::U8 => "SMALLINT",
        Type::U16 => "INT",
        Type::U32 => "BIGINT",
        Type::U64 => "DECIMAL(20, 0)",
        Type::F32 => "REAL",
        Type::F64 => "FLOAT",
        Type::String => "NVARCHAR(MAX)",
        Type::Uuid => "UNIQUEIDENTIFIER",
        #[cfg(feature = "rust_decimal")]
        Type::Decimal => "DECIMAL(38, 10)",
        #[cfg(feature = "bigdecimal")]
        Type::BigDecimal => "DECIMAL(38, 10)",
        #[cfg(feature = "jiff")]
        Type::Timestamp => "DATETIME2(6)",
        #[cfg(feature = "jiff")]
        Type::Date => "DATE",
        #[cfg(feature = "jiff")]
        Type::Time => "TIME(6)",
        #[cfg(feature = "jiff")]
        Type::DateTime => "DATETIME2(6)",
        _ => return None,
    })
}

/// The fields of a row value (`Expr::Record`), if `expr` is one.
fn row_fields(expr: &stmt::Expr) -> Option<&[stmt::Expr]> {
    match expr {
        stmt::Expr::Record(record) => Some(&record.fields),
        _ => None,
    }
}

/// The positional alias T-SQL uses for the `index`th column of a derived table.
fn mssql_column_alias(index: usize) -> String {
    format!("column{}", index + 1)
}

/// Renders `(a, b) IN ((x, y), (z, w))` for SQL Server.
///
/// T-SQL has no row value constructor, so a composite `IN` list becomes the
/// equivalent `OR` of `AND`s. `NULL` handling carries over unchanged: an
/// unknown comparison stays unknown through `AND` and `OR`.
fn serialize_mssql_row_in_list(
    f: &mut super::Formatter<'_>,
    fields: &[stmt::Expr],
    list: &stmt::Expr,
) {
    let rows: Vec<&stmt::Expr> = match list {
        stmt::Expr::List(list) => list.items.iter().collect(),
        other => panic!("SQL Server requires a composite key IN list of tuples, found {other:?}"),
    };

    if rows.is_empty() {
        // An empty list matches nothing, but T-SQL still needs a boolean
        // expression wherever the planner put this one.
        f.dst.push_str("(1 = 0)");
        return;
    }

    f.dst.push('(');
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            f.dst.push_str(" OR ");
        }

        let Some(row) = row_fields(row) else {
            panic!("SQL Server requires each element of a composite key IN list to be a tuple");
        };
        assert_eq!(
            row.len(),
            fields.len(),
            "composite key tuples must have matching arity"
        );

        f.dst.push('(');
        for (index, (lhs, rhs)) in fields.iter().zip(row).enumerate() {
            if index > 0 {
                f.dst.push_str(" AND ");
            }
            lhs.to_sql(f);
            f.dst.push_str(" = ");
            rhs.to_sql(f);
        }
        f.dst.push(')');
    }
    f.dst.push(')');
}

/// Renders `(a, b) IN (SELECT x, y FROM …)` for SQL Server.
///
/// A row value cannot be compared against a subquery either, so the subquery is
/// wrapped in a derived table. The explicit column list gives it the positional
/// `column1`, `column2` names the projection already aliases its select list
/// with, which the correlation then compares against.
fn serialize_mssql_row_in_subquery(
    f: &mut super::Formatter<'_>,
    fields: &[stmt::Expr],
    query: &stmt::Query,
    negated: bool,
) {
    // `depth` is bumped by every nested query, so sibling subqueries cannot
    // collide and neither can two nested composite `IN`s.
    let alias = format!("in_{}", f.depth);

    if negated {
        f.dst.push_str("NOT ");
    }
    f.dst.push_str("EXISTS (SELECT 1 FROM (");
    query.to_sql(f);
    f.dst.push_str(") AS [");
    f.dst.push_str(&alias);
    f.dst.push_str("] (");
    for index in 0..fields.len() {
        if index > 0 {
            f.dst.push_str(", ");
        }
        f.dst.push_str(&format!("[{}]", mssql_column_alias(index)));
    }
    f.dst.push_str(") WHERE ");

    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            f.dst.push_str(" AND ");
        }
        f.dst
            .push_str(&format!("[{alias}].[{}] = ", mssql_column_alias(index)));
        field.to_sql(f);
    }

    f.dst.push(')');
}

impl ToSql for &stmt::BinaryOp {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        f.dst.push_str(match self {
            stmt::BinaryOp::Eq => "=",
            stmt::BinaryOp::Gt => ">",
            stmt::BinaryOp::Ge => ">=",
            stmt::BinaryOp::Lt => "<",
            stmt::BinaryOp::Le => "<=",
            stmt::BinaryOp::Ne => "<>",
            stmt::BinaryOp::Add => "+",
            stmt::BinaryOp::Sub => "-",
        })
    }
}
