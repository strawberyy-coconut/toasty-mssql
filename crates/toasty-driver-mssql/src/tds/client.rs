//! Connecting to SQL Server and running statements over TDS.

use mssql_tds::{
    connection::{
        client_context::ClientContext,
        tds_client::{ResultSet as TdsResultSet, StatementResult, TdsClient},
    },
    connection_provider::tds_connection_provider::TdsConnectionProvider,
    datatypes::column_values::ColumnValues,
    error::Error as TdsError,
    message::{
        parameters::rpc_parameters::RpcParameter, transaction_management::TransactionIsolationLevel,
    },
    query::metadata::ColumnMetadata,
};
use toasty_core::{Error, Result, driver::operation::IsolationLevel};

/// A single result set: its column metadata and its rows.
#[derive(Debug)]
pub(crate) struct ResultSet {
    pub(crate) columns: Vec<ColumnMetadata>,
    pub(crate) rows: Vec<Vec<ColumnValues>>,
}

/// What a statement execution produced.
#[derive(Debug)]
pub(crate) enum ExecOutcome {
    /// The statement returned no rows; the value is the affected-row count.
    Count(u64),
    /// The statement returned a result set.
    Rows(ResultSet),
}

/// An open TDS connection.
pub(crate) struct Client {
    client: Box<TdsClient>,
    /// Current transaction nesting depth. SQL Server has no nested
    /// transactions, so nesting is emulated with savepoints.
    transaction_depth: usize,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("transaction_depth", &self.transaction_depth)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Opens a connection from a `mssql-tds` configuration.
    ///
    /// The data source the context names is passed alongside it, which is what
    /// `create_client` asks for; taking them from the same value keeps the two
    /// from disagreeing.
    pub(crate) async fn connect(context: &ClientContext) -> Result<Self> {
        // `mssql-tds`'s client-creation future is deeply nested. Awaiting it
        // unboxed splices that type into this future, where it counts against
        // the recursion limit of every crate that connects. Boxing type-erases
        // it. The client itself is also large, so it is boxed too.
        let client = Box::pin(TdsConnectionProvider.create_client(
            context.clone(),
            &context.data_source,
            None,
        ))
        .await
        .map_err(classify)?;

        Ok(Self {
            client: Box::new(client),
            transaction_depth: 0,
        })
    }

    /// Runs a statement and collects the rows of its first row-returning result
    /// set, or the affected-row count when it returns no rows.
    ///
    /// `expect_rows` selects how a statement that returns no result set is
    /// reported; either way the whole batch is drained before returning, so the
    /// connection is left idle.
    pub(crate) async fn exec(
        &mut self,
        sql: String,
        params: Vec<RpcParameter>,
        expect_rows: bool,
    ) -> Result<ExecOutcome> {
        let result = self.send(sql, params).await?;

        if expect_rows {
            return self.collect_rows(result).await;
        }

        self.drain(result).await
    }

    /// Sends a statement, returning the first result of its batch.
    async fn send(&mut self, sql: String, params: Vec<RpcParameter>) -> Result<StatementResult> {
        if params.is_empty() {
            self.client.execute(sql, ()).await.map_err(classify)
        } else {
            self.client
                .execute_sp_executesql(sql, params, ())
                .await
                .map_err(classify)
        }
    }

    /// Advances to the first row-returning result set and reads it.
    async fn collect_rows(&mut self, result: StatementResult) -> Result<ExecOutcome> {
        if !matches!(result, StatementResult::Rows) {
            // `execute` stops at the first statement boundary; only advance when
            // it was not already a row-returning result set, otherwise the rows
            // just handed to us would be skipped.
            let positioned = self.client.advance_to_rows().await.map_err(classify)?;

            if !positioned {
                self.client.close_query().await.ok();
                return Ok(ExecOutcome::Rows(ResultSet {
                    columns: Vec::new(),
                    rows: Vec::new(),
                }));
            }
        }

        let columns = self.client.get_metadata().clone();
        let mut rows = Vec::new();

        while let Some(values) = self.client.next_row().await.map_err(classify)? {
            rows.push(values);
        }

        self.client.close_query().await.map_err(classify)?;

        Ok(ExecOutcome::Rows(ResultSet { columns, rows }))
    }

    /// Advances to the end of the batch, returning the last affected-row count.
    async fn drain(&mut self, mut result: StatementResult) -> Result<ExecOutcome> {
        let mut affected = None;

        while !matches!(result, StatementResult::End) {
            if let StatementResult::NoRows { rows_affected } = result
                && let Some(count) = rows_affected
            {
                affected = Some(count);
            }

            result = self.client.advance().await.map_err(classify)?;
        }

        self.client.close_query().await.map_err(classify)?;

        Ok(ExecOutcome::Count(affected.unwrap_or(0)))
    }

    /// Begins a transaction, or a savepoint when one is already open.
    pub(crate) async fn begin(
        &mut self,
        isolation: Option<IsolationLevel>,
        read_only: bool,
    ) -> Result<()> {
        // SQL Server has no read-only transaction; the flag is accepted and
        // ignored rather than failing a query the engine planned.
        let _ = read_only;

        if self.transaction_depth == 0 {
            let level = match isolation {
                Some(IsolationLevel::ReadUncommitted) => TransactionIsolationLevel::ReadUncommitted,
                None | Some(IsolationLevel::ReadCommitted) => {
                    TransactionIsolationLevel::ReadCommitted
                }
                Some(IsolationLevel::RepeatableRead) => TransactionIsolationLevel::RepeatableRead,
                Some(IsolationLevel::Serializable) => TransactionIsolationLevel::Serializable,
            };

            self.client
                .begin_transaction(level, None)
                .await
                .map_err(classify)?;
        } else {
            // Nesting is emulated with a savepoint so an inner rollback only
            // undoes the inner work.
            self.client
                .save_transaction(savepoint_name(self.transaction_depth))
                .await
                .map_err(classify)?;
        }

        self.transaction_depth += 1;
        Ok(())
    }

    /// Commits the innermost transaction. Only the outermost commit ends the
    /// database transaction.
    pub(crate) async fn commit(&mut self) -> Result<()> {
        if self.transaction_depth == 0 {
            return Ok(());
        }

        if self.transaction_depth == 1 {
            self.client
                .commit_transaction(None, None)
                .await
                .map_err(classify)?;
        }

        self.transaction_depth -= 1;
        Ok(())
    }

    /// Rolls back the innermost transaction, or to its savepoint when nested.
    pub(crate) async fn rollback(&mut self) -> Result<()> {
        if self.transaction_depth == 0 {
            return Ok(());
        }

        if self.transaction_depth == 1 {
            self.client
                .rollback_transaction(None, None)
                .await
                .map_err(classify)?;
        } else {
            self.client
                .rollback_transaction(Some(savepoint_name(self.transaction_depth - 1)), None)
                .await
                .map_err(classify)?;
        }

        self.transaction_depth -= 1;
        Ok(())
    }

    /// Creates a savepoint with the given name.
    pub(crate) async fn savepoint(&mut self, name: &str) -> Result<()> {
        self.client
            .save_transaction(name.to_owned())
            .await
            .map_err(classify)
    }

    /// Rolls back to a named savepoint, leaving the transaction open.
    pub(crate) async fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        self.client
            .rollback_transaction(Some(name.to_owned()), None)
            .await
            .map_err(classify)
    }

    /// Whether the connection still looks alive.
    pub(crate) fn is_valid(&self) -> bool {
        !self.client.is_connection_dead()
    }
}

/// The savepoint name used for the given nesting depth.
fn savepoint_name(depth: usize) -> String {
    format!("toasty_sp_{depth}")
}

/// Maps a TDS error onto Toasty's error taxonomy.
///
/// Per `toasty_core::driver`'s contract, a conflict that a retry could resolve is
/// reported as a serialization failure rather than a generic driver error, and a
/// transport failure is reported as a lost connection so the pool evicts the
/// slot instead of reusing a dead socket.
pub(crate) fn classify(error: TdsError) -> Error {
    let (number, message) = match &error {
        TdsError::SqlServerError { diagnostics } => match diagnostics.errors.first() {
            Some(first) => (Some(i64::from(first.number)), first.message.clone()),
            None => (None, error.to_string()),
        },
        _ => (None, error.to_string()),
    };

    match number {
        // Deadlock victim, and snapshot-isolation update conflict.
        Some(1205) | Some(3960) => Error::serialization_failure(message),
        // Lock request timeout. The statement failed only because it could not
        // take a lock within `LOCK_TIMEOUT`, so the same statement can succeed
        // on a retry — which is the contract `serialization_failure` documents
        // for a retryable conflict. Reported as an opaque driver error, a caller
        // or a retry loop would have no way to tell it apart from a real fault.
        Some(1222) => Error::serialization_failure(message),
        // Attempted write on a read-only database.
        Some(3906) => Error::read_only_transaction(message),
        // A server-reported error that Toasty has no more specific category for.
        Some(_) => Error::driver_operation_failed(error),
        // Anything else came from the transport or protocol layer rather than
        // from SQL Server, which the pool treats as an evictable connection.
        None => Error::connection_lost(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `mssql-tds` configuration for the test server.
    ///
    /// The integration tests build the same thing in `tests/common`, which a
    /// unit test cannot reach, so the little it needs is repeated here.
    fn context() -> ClientContext {
        use crate::MssqlConnectOptions;

        MssqlConnectOptions::parse(&env("DATABASE_URL", DEFAULT_DATABASE_URL))
            .expect("DATABASE_URL must be a valid mssql:// URL")
            .to_client_context()
    }

    /// The development server, as the URL these tests connect with unless
    /// `DATABASE_URL` says otherwise.
    const DEFAULT_DATABASE_URL: &str =
        "mssql://sa:Password1!@db:1433/testdb?encrypt=on&trust_certificate=true";

    /// An environment variable, or `default` when it is unset.
    fn env(name: &str, default: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| default.to_owned())
    }

    /// Connects to the configured database, creating it first if needed.
    ///
    /// The SQL Server container ships with `master` and `tempdb` only, so the
    /// database named in the URL has to be created before it can be connected
    /// to.
    async fn connect(context: &ClientContext) -> Client {
        create_database_if_missing(context).await;
        Client::connect(context)
            .await
            .expect("connect must succeed")
    }

    async fn create_database_if_missing(context: &ClientContext) {
        // Tests run in parallel, so two of them could race on `CREATE
        // DATABASE`. Serialize the whole check-then-create across the process.
        static CREATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _guard = CREATE_LOCK.lock().await;

        let mut master = context.clone();
        master.database = "master".to_owned();

        let mut master = Client::connect(&master)
            .await
            .expect("connecting to `master` must succeed");

        let name = context.database.replace(']', "]]");

        // `CREATE DATABASE` has to be the only statement in its batch, so it
        // runs inside dynamic SQL. Identifiers cannot be bind parameters.
        master
            .exec(
                format!("IF DB_ID(N'{name}') IS NULL EXEC(N'CREATE DATABASE [{name}]')"),
                Vec::new(),
                false,
            )
            .await
            .expect("creating the database must succeed");
    }

    /// The transport smoke test. Requires the SQL Server container from
    /// `compose.dev.yaml`, reachable through the `MSSQL_*` variables below.
    #[tokio::test]
    async fn connects_and_selects() {
        let mut client = connect(&context()).await;

        let outcome = client
            .exec("SELECT 1 AS one".to_owned(), Vec::new(), true)
            .await
            .expect("SELECT must succeed");

        let ExecOutcome::Rows(result) = outcome else {
            panic!("expected a result set");
        };

        assert_eq!(result.columns.len(), 1);
        assert_eq!(result.rows.len(), 1);
    }

    /// A DDL statement with no result set reports an affected-row count.
    #[tokio::test]
    async fn executes_ddl() {
        let mut client = connect(&context()).await;

        client
            .exec(
                "DROP TABLE IF EXISTS dbo.__toasty_smoke".to_owned(),
                Vec::new(),
                false,
            )
            .await
            .expect("DROP must succeed");

        let outcome = client
            .exec(
                "CREATE TABLE dbo.__toasty_smoke (id BIGINT NOT NULL PRIMARY KEY, name NVARCHAR(4000) NOT NULL)"
                    .to_owned(),
                Vec::new(),
                false,
            )
            .await
            .expect("CREATE TABLE must succeed");

        assert!(matches!(outcome, ExecOutcome::Count(_)));

        client
            .exec(
                "DROP TABLE dbo.__toasty_smoke".to_owned(),
                Vec::new(),
                false,
            )
            .await
            .expect("DROP must succeed");
    }
}
