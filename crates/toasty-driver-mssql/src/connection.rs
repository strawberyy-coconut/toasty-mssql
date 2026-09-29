//! The [`Connection`](toasty_core::driver::Connection) implementation:
//! executing operations, applying DDL and reporting applied migrations.

use async_trait::async_trait;
use mssql_tds::message::parameters::rpc_parameters::RpcParameter;
use toasty_core::{
    Error, Result, Schema,
    driver::{
        Capability, ExecResponse, Operation, QueryLogConfig,
        log::QueryLog,
        operation::{RawSqlRet, Transaction, TransactionMode, TypedValue},
    },
    schema::{db, db::AppliedMigration},
    stmt,
};
use toasty_sql::{Serializer, stmt as sql};

use crate::tds::{Client, ExecOutcome, ResultSet, decode, params};

/// The table migration history is recorded in.
///
/// Toasty never sees this: it exists so `applied_migrations` can answer, and so
/// applying a migration twice is detectable. The name and shape match the other
/// SQL drivers' history table.
const MIGRATION_TABLE: &str = "__toasty_migrations";

/// An open connection to a SQL Server database.
pub struct Connection {
    client: Client,

    /// Configuration for the `toasty::query` event, copied from the
    /// [`ConnectContext`](toasty_core::driver::ConnectContext) when the pool
    /// opens the connection.
    pub(crate) query_log: QueryLogConfig,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("client", &self.client)
            .field("query_log", &self.query_log)
            .finish()
    }
}

impl Connection {
    pub(crate) fn new(client: Client) -> Self {
        Self {
            client,
            query_log: QueryLogConfig::default(),
        }
    }
}

/// How a statement's result should be interpreted.
enum Ret {
    /// The statement returns no rows; report an affected-row count.
    Count,

    /// The statement returns rows whose types the plan does not name.
    Infer,

    /// The statement returns rows with these types, in order.
    Types(Vec<stmt::Type>),
}

#[async_trait]
impl toasty_core::driver::Connection for Connection {
    async fn exec(
        &mut self,
        schema: &std::sync::Arc<Schema>,
        op: Operation,
    ) -> Result<ExecResponse> {
        let (sql, rpc_params, ret) = match &op {
            Operation::Insert(insert) => (
                Serializer::mssql(&schema.db).serialize(&sql::Statement::from(insert.stmt.clone())),
                params::rpc_params(&insert.params)?,
                match &insert.ret {
                    Some(types) => Ret::Types(types.clone()),
                    None => Ret::Count,
                },
            ),
            Operation::QuerySql(query) => (
                Serializer::mssql(&schema.db).serialize(&sql::Statement::from(query.stmt.clone())),
                params::rpc_params(&query.params)?,
                match &query.ret {
                    Some(types) => Ret::Types(types.clone()),
                    None => Ret::Count,
                },
            ),
            Operation::RawSql(raw) => {
                // The caller writes `?` markers, which `sp_executesql` names
                // `@p1`, `@p2`, …
                let (sql, _markers) = params::rewrite_markers(&raw.sql);

                let ret = match &raw.ret {
                    RawSqlRet::None => Ret::Count,
                    RawSqlRet::Infer => Ret::Infer,
                    RawSqlRet::Types(types) => Ret::Types(types.clone()),
                };

                (sql, params::rpc_params(&raw.params)?, ret)
            }
            Operation::Transaction(transaction) => return self.transaction(transaction).await,
            other => {
                return Err(Error::unsupported_feature(format!(
                    "SQL Server driver does not support the `{}` operation",
                    other.name()
                )));
            }
        };

        tracing::trace!(driver = "mssql", op = op.name(), "driver exec");

        // A debugging affordance: the conformance suite reports failures as
        // database errors, and the generated SQL is what explains them. Set
        // `MSSQL_TRACE_SQL=1` to see every statement.
        if std::env::var_os("MSSQL_TRACE_SQL").is_some() {
            eprintln!("MSSQL[{}] {}", op.name(), sql);
        }

        // The parameters the query log renders are the operation's own, before
        // they are converted to the wire form the client sends.
        let log_params: &[TypedValue] = match &op {
            Operation::Insert(op) => &op.params,
            Operation::QuerySql(op) => &op.params,
            Operation::RawSql(op) => &op.params,
            _ => &[],
        };

        let expect_rows = !matches!(ret, Ret::Count);

        // `QueryLog` borrows the SQL text, while the client takes it by value.
        // When nothing is observing the event the log is skipped, so the text
        // can be moved rather than copied.
        let observing = tracing::event_enabled!(target: "toasty::query", tracing::Level::DEBUG)
            || tracing::event_enabled!(target: "toasty::query", tracing::Level::WARN);

        if observing {
            let log_sql = sql.clone();
            let log = QueryLog::sql(
                &self.query_log,
                "mssql",
                &log_sql,
                log_params.iter().map(|typed| &typed.value),
            );

            let result = self.execute(sql, rpc_params, expect_rows, &ret).await;
            log.finish(&result);
            result
        } else {
            self.execute(sql, rpc_params, expect_rows, &ret).await
        }
    }

    async fn push_schema(&mut self, schema: &Schema) -> Result<()> {
        for table in &schema.db.tables {
            tracing::debug!(table = %table.name, "creating table");

            let serializer = Serializer::mssql(&schema.db);
            let sql =
                serializer.serialize(&sql::Statement::create_table(table, &Capability::MSSQL));
            self.client.exec(sql, Vec::new(), false).await?;

            for index in &table.indices {
                if index.primary_key {
                    continue;
                }

                let sql = serializer.serialize(&sql::Statement::create_index(index));
                self.client.exec(sql, Vec::new(), false).await?;
            }
        }

        Ok(())
    }

    async fn applied_migrations(&mut self) -> Result<Vec<AppliedMigration>> {
        self.ensure_migration_table().await?;

        let outcome = self
            .client
            .exec(
                format!("SELECT [id] FROM {MIGRATION_TABLE} ORDER BY [applied_at]"),
                Vec::new(),
                true,
            )
            .await?;

        let ExecOutcome::Rows(result) = outcome else {
            return Err(Error::invalid_result(
                "the migration history query returned no rows",
            ));
        };

        decode_rows(&result, &Ret::Types(vec![stmt::Type::I64]))?
            .into_iter()
            .map(|row| {
                // Every row arrives as a record, even a single-column one.
                let id = match row {
                    stmt::Value::Record(record) => record.into_iter().next().ok_or_else(|| {
                        Error::invalid_result("a migration history row had no columns")
                    })?,
                    value => value,
                };

                match id {
                    stmt::Value::I64(id) => {
                        u64::try_from(id).map(AppliedMigration::new).map_err(|_| {
                            Error::invalid_result(format!("migration id {id} is negative"))
                        })
                    }
                    other => Err(Error::invalid_result(format!(
                        "expected a migration id, found {other:?}"
                    ))),
                }
            })
            .collect()
    }

    fn is_valid(&self) -> bool {
        // Cheap, local check the pool consults when a connection is returned.
        self.client.is_valid()
    }

    async fn ping(&mut self) -> Result<()> {
        // `SELECT 1` is the cheapest round trip SQL Server offers; the TDS
        // client exposes no protocol-level ping.
        //
        // This overrides the trait's default, which does no I/O and is only
        // right for a backend that cannot fail in isolation (in-process SQLite)
        // or that pools beneath this surface. This driver holds a socket to a
        // server that can go away while the connection sits idle, and the pool's
        // health-check sweep calls exactly this method to find that out —
        // `is_valid` above is a passive signal, so without a probe a killed
        // connection is only discovered when a caller's operation fails.
        //
        // Per the trait contract a failure here is `connection_lost`, not a
        // generic operation error: the pool branches on that classification to
        // evict the slot rather than put it back in the idle set.
        let result = self
            .client
            .exec("SELECT 1".to_owned(), Vec::new(), true)
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(error) if error.is_connection_lost() => Err(error),
            Err(error) => Err(Error::connection_lost(error)),
        }
    }

    async fn apply_migration(
        &mut self,
        id: u64,
        name: &str,
        migration: &toasty_core::schema::db::Migration,
    ) -> Result<()> {
        let toasty_core::schema::db::Migration::Sql(sql) = migration;

        self.ensure_migration_table().await?;

        // Statements are separated by the marker
        // `Migration::new_sql_with_breakpoints` writes and by a `GO` line, not by
        // semicolons: `;` can appear inside a string literal or a trigger body,
        // and splitting on it would cut the statement in half.
        let statements = crate::migration::batches(sql);

        // A migration is all or nothing, and SQL Server has transactional DDL,
        // so a failure part way through leaves the schema untouched.
        self.client.begin(Default::default(), false).await?;

        let result = async {
            for statement in &statements {
                self.client
                    .exec(statement.clone(), Vec::new(), false)
                    .await?;
            }

            let params = params::rpc_params(&[
                TypedValue {
                    value: stmt::Value::I64(i64::try_from(id).map_err(|_| {
                        Error::invalid_result(format!("migration id {id} does not fit in an i64"))
                    })?),
                    ty: db::Type::Integer(8),
                },
                TypedValue {
                    value: stmt::Value::String(name.to_owned()),
                    ty: db::Type::VarChar(255),
                },
            ])?;

            self.client
                .exec(
                    format!(
                        "INSERT INTO {MIGRATION_TABLE} ([id], [name], [applied_at]) \
                         VALUES (@p1, @p2, SYSUTCDATETIME())"
                    ),
                    params,
                    false,
                )
                .await?;

            Ok(())
        }
        .await;

        match result {
            Ok(()) => self.client.commit().await,
            Err(error) => {
                // The transaction may already be gone if the failure aborted
                // it, which is why the rollback result is dropped.
                let _ = self.client.rollback().await;
                Err(error)
            }
        }
    }
}

impl Connection {
    /// Sends one statement and converts the outcome per `ret`.
    async fn execute(
        &mut self,
        sql: String,
        rpc_params: Vec<RpcParameter>,
        expect_rows: bool,
        ret: &Ret,
    ) -> Result<ExecResponse> {
        let outcome = self.client.exec(sql, rpc_params, expect_rows).await?;

        match outcome {
            ExecOutcome::Count(count) => Ok(ExecResponse::count(count)),
            ExecOutcome::Rows(result) => {
                let values = decode_rows(&result, ret)?;
                Ok(ExecResponse::value_stream(stmt::ValueStream::from_vec(
                    values,
                )))
            }
        }
    }

    /// Creates the migration history table if it is not there yet.
    ///
    /// Both the read and the write path call this, so the table exists whatever
    /// order Toasty asks in, and a database whose history was lost heals on the
    /// next run.
    async fn ensure_migration_table(&mut self) -> Result<()> {
        self.client
            .exec(
                format!(
                    "IF OBJECT_ID(N'{MIGRATION_TABLE}', N'U') IS NULL \
                     CREATE TABLE {MIGRATION_TABLE} ( \
                         [id] BIGINT NOT NULL PRIMARY KEY, \
                         [name] NVARCHAR(255) NOT NULL, \
                         [applied_at] DATETIME2 NOT NULL \
                     )"
                ),
                Vec::new(),
                false,
            )
            .await?;

        Ok(())
    }

    /// Runs one transaction-control operation.
    async fn transaction(&mut self, transaction: &Transaction) -> Result<ExecResponse> {
        match transaction {
            Transaction::Start {
                isolation,
                read_only,
                mode,
            } => {
                // T-SQL has no `BEGIN IMMEDIATE`/`EXCLUSIVE` analogue, and Toasty's
                // contract is to reject a mode the backend cannot honour rather
                // than silently downgrade it.
                if !matches!(mode, TransactionMode::Default) {
                    return Err(Error::unsupported_feature(format!(
                        "SQL Server does not support TransactionMode::{mode:?}"
                    )));
                }

                self.client.begin(*isolation, *read_only).await?;
            }
            Transaction::Commit => self.client.commit().await?,
            Transaction::Rollback => self.client.rollback().await?,
            Transaction::Savepoint(name) => self.client.savepoint(name).await?,
            Transaction::RollbackToSavepoint(name) => {
                self.client.rollback_to_savepoint(name).await?
            }
            // SQL Server has no `RELEASE SAVEPOINT`: a savepoint is discarded
            // when the transaction commits, and there is no way to release one
            // earlier.
            Transaction::ReleaseSavepoint(_) => {}
        }

        Ok(ExecResponse::count(0))
    }
}

/// Converts a result set into Toasty records.
///
/// Every driver returns one `Value::Record` per row, with fields in the order
/// the plan projected them.
fn decode_rows(result: &ResultSet, ret: &Ret) -> Result<Vec<stmt::Value>> {
    result
        .rows
        .iter()
        .map(|row| {
            // The result metadata and the row must agree; a mismatch means the
            // cursor was advanced inconsistently.
            if row.len() != result.columns.len() {
                return Err(Error::invalid_result(format!(
                    "result set reported {} column(s) but the row has {}",
                    result.columns.len(),
                    row.len()
                )));
            }

            let fields = row
                .iter()
                .enumerate()
                .map(|(index, value)| match ret {
                    Ret::Types(types) => {
                        let ty = types.get(index).ok_or_else(|| {
                            Error::invalid_result(format!(
                                "plan named {} column type(s) but the result has {} column(s)",
                                types.len(),
                                row.len()
                            ))
                        })?;

                        decode::decode(value, ty)
                    }
                    Ret::Infer => decode::infer(value),
                    Ret::Count => unreachable!("count results carry no rows"),
                })
                .collect::<Result<Vec<_>>>()?;

            Ok(stmt::ValueRecord::from_vec(fields).into())
        })
        .collect()
}
