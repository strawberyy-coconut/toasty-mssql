//! The [`Driver`] implementation: connection creation, and the schema-level
//! operations.

use std::borrow::Cow;
use std::str::FromStr;

use async_trait::async_trait;
use mssql_tds::connection::client_context::ClientContext;
use toasty_core::{
    Error, Result,
    driver::{Capability, ConnectContext, Driver},
    schema::{db::Migration, diff},
};

use crate::{connection::Connection, migration, options::MssqlConnectOptions, tds};

/// A SQL Server [`Driver`] that speaks TDS through `mssql-tds`.
///
/// It is built either from an `mssql://` URL — [`from_url`](Self::from_url), or
/// `str::parse` — or from `mssql-tds`'s own connection configuration, so every
/// option that crate offers is reachable without this driver restating any of
/// them:
///
/// ```no_run
/// use toasty_driver_mssql::Mssql;
///
/// let driver = Mssql::from_url(
///     "mssql://sa:Password1!@localhost:1433/mydb?encrypt=on&trust_certificate=true",
/// )
/// .expect("the URL must parse");
/// ```
///
/// ```no_run
/// use toasty_driver_mssql::{ClientContext, EncryptionOptions, EncryptionSetting, Mssql};
///
/// let mut context = ClientContext::with_data_source("tcp:localhost,1433");
/// context.user_name = "sa".to_owned();
/// context.password = "Password1!".to_owned();
/// context.database = "mydb".to_owned();
/// context.encryption_options = EncryptionOptions {
///     // `ClientContext`'s own default is `Strict`, which is TDS 8.0.
///     mode: EncryptionSetting::On,
///     trust_server_certificate: true,
///     ..Default::default()
/// };
///
/// let driver = Mssql::new(context);
/// ```
pub struct Mssql {
    /// Reused for every connection the driver opens, so one set of credentials
    /// and one set of options serves the whole pool.
    context: ClientContext,

    /// The URL the driver was built from, when it was built from one. A driver
    /// built from a [`ClientContext`] was never given a URL, and reports its
    /// data source instead.
    url: Option<String>,
}

impl std::fmt::Debug for Mssql {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `ClientContext` has no `Debug` of its own, and it holds the password,
        // so the fields worth seeing are named one at a time.
        f.debug_struct("Mssql")
            .field("data_source", &self.context.data_source)
            .field("database", &self.context.database)
            .field("user_name", &self.context.user_name)
            .finish_non_exhaustive()
    }
}

impl Mssql {
    /// Creates a driver from a `mssql-tds` connection configuration.
    ///
    /// Nothing is validated here: the configuration is handed to `mssql-tds`
    /// when a connection is opened, which is where it can be reported properly.
    pub fn new(context: ClientContext) -> Self {
        Self { context, url: None }
    }

    /// Creates a driver from parsed [`MssqlConnectOptions`].
    ///
    /// The options name no URL, so [`Driver::url`] reports the data source they
    /// describe.
    pub fn from_options(options: &MssqlConnectOptions) -> Self {
        Self {
            context: options.to_client_context(),
            url: None,
        }
    }

    /// Creates a driver from an `mssql://` URL.
    ///
    /// The URL is kept, so [`Driver::url`] reports it again — which is what a
    /// caller redacting a password for display needs.
    ///
    /// ```no_run
    /// use toasty_driver_mssql::Mssql;
    ///
    /// let driver = Mssql::from_url("mssql://sa:Password1!@localhost:1433/mydb?encrypt=on")
    ///     .expect("the URL must parse");
    /// ```
    pub fn from_url(url: &str) -> Result<Self> {
        let options = MssqlConnectOptions::parse(url)?;

        Ok(Self {
            context: options.to_client_context(),
            url: Some(url.to_owned()),
        })
    }

    /// Connects to a different database on the same server.
    async fn connect_database(&self, database: &str) -> Result<tds::Client> {
        let mut context = self.context.clone();
        context.database = database.to_owned();

        tds::Client::connect(&context).await
    }

    /// Runs a single statement that takes no parameters, discarding its result.
    ///
    /// This is the escape hatch for DDL that runs outside the query engine —
    /// the integration suite's per-test table cleanup, for example.
    pub async fn execute_raw(&self, sql: &str) -> Result<()> {
        let mut client = tds::Client::connect(&self.context).await?;
        client.exec(sql.to_owned(), Vec::new(), false).await?;
        Ok(())
    }
}

impl FromStr for Mssql {
    type Err = Error;

    /// Parses an `mssql://` URL, as [`Mssql::from_url`] does.
    fn from_str(url: &str) -> Result<Self> {
        Self::from_url(url)
    }
}

#[async_trait]
impl Driver for Mssql {
    fn url(&self) -> Cow<'_, str> {
        // A driver built from a `ClientContext` need never have seen a URL: what
        // it holds is `mssql-tds`'s data source (`tcp:host,1433`), which is the
        // closest thing to one. A driver built from a URL reports that instead.
        match &self.url {
            Some(url) => Cow::Borrowed(url),
            None => Cow::Borrowed(&self.context.data_source),
        }
    }

    fn capability(&self) -> &'static Capability {
        &Capability::MSSQL
    }

    async fn connect(
        &self,
        cx: &ConnectContext,
    ) -> Result<Box<dyn toasty_core::driver::Connection>> {
        let client = tds::Client::connect(&self.context).await?;

        let mut connection = Connection::new(client);
        connection.query_log = cx.query_log;

        Ok(Box::new(connection))
    }

    fn generate_migration(&self, diff: &diff::Schema<'_>) -> Migration {
        // Toasty computes the diff and the built-in migration engine turns it
        // into a list of DDL statements; the SQL Server serializer renders
        // each one. The trait cannot report an error, and a migration that
        // quietly did nothing would still be recorded as applied, so an
        // unrenderable diff becomes SQL that raises when it runs.
        let statements =
            toasty_sql::migration::MigrationStatement::from_diff(diff, &Capability::MSSQL);

        match render_migration(&statements) {
            Ok(statements) => Migration::new_sql_with_breakpoints(statements.as_slice()),
            Err(error) => Migration::new_sql(migration::unrenderable(&error)),
        }
    }

    async fn reset_db(&self) -> Result<()> {
        // A URL always names a database, but a context assembled by hand need
        // not — and `DROP DATABASE []` would be a syntax error rather than an
        // answer.
        let database = &self.context.database;

        if database.is_empty() {
            return Err(Error::invalid_driver_configuration(
                "cannot reset a database when the connection configuration names none",
            ));
        }

        let name = database.replace(']', "]]");

        // DROP DATABASE cannot run while the caller is connected to the
        // database, and CREATE DATABASE must be the only statement in its
        // batch, so both go through `master` as dynamic SQL.
        let mut master = self.connect_database("master").await?;

        master
            .exec(
                format!(
                    "IF DB_ID(N'{name}') IS NOT NULL BEGIN \
                     ALTER DATABASE [{name}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; \
                     DROP DATABASE [{name}]; \
                     END"
                ),
                Vec::new(),
                false,
            )
            .await?;

        master
            .exec(
                format!("EXEC(N'CREATE DATABASE [{name}]')"),
                Vec::new(),
                false,
            )
            .await?;

        Ok(())
    }
}

/// Renders each migration statement with the SQL Server serializer, against the
/// schema snapshot the statement was generated for.
fn render_migration(
    statements: &[toasty_sql::migration::MigrationStatement],
) -> Result<Vec<String>> {
    statements
        .iter()
        .map(|statement| {
            Ok(toasty_sql::Serializer::mssql(statement.schema()).serialize(statement.statement()))
        })
        .collect()
}
