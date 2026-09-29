/// The SQL dialect a driver speaks.
///
/// Everything Toasty renders as SQL is a function of the dialect, not of the
/// live connection, so the dialect is named on
/// [`Capability::sql`](super::Capability::sql) rather than discovered from a
/// driver at runtime.
///
/// Dialect-compatible engines share a variant: Turso reports
/// [`Sqlite`](Self::Sqlite) because it accepts SQLite's SQL.
///
/// [`Mssql`](Self::Mssql) is SQL Server's T-SQL. It is the dialect that differs
/// most from the others: identifiers are `[bracket]` quoted, bind parameters are
/// named `@pN`, `LIMIT` is spelled `OFFSET … FETCH NEXT`, and `RETURNING`
/// becomes an `OUTPUT INSERTED`/`DELETED` clause.
///
/// # Examples
///
/// ```
/// use toasty_core::driver::{Capability, Dialect};
///
/// assert_eq!(Capability::SQLITE.sql, Some(Dialect::Sqlite));
/// assert_eq!(Capability::DYNAMODB.sql, None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dialect {
    /// SQLite, and engines that accept SQLite's SQL.
    Sqlite,

    /// PostgreSQL.
    Postgresql,

    /// MySQL.
    Mysql,

    /// MariaDB.
    MariaDb,

    /// Microsoft SQL Server, and engines that accept T-SQL.
    Mssql,
}
