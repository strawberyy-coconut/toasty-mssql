//! Toasty driver for [Microsoft SQL Server](https://www.microsoft.com/sql-server),
//! speaking the TDS protocol directly through Microsoft's
//! [`mssql-tds`](https://github.com/microsoft/mssql-rs) client. No ODBC, no
//! `sqlx`, no background thread bridging to a blocking API.
//!
//! SQL Server is the [`Mssql`](toasty_core::driver::Dialect::Mssql) dialect: its
//! statements are rendered by [`toasty_sql`], the same serializer the other SQL
//! drivers use, and its capabilities are
//! [`Capability::MSSQL`](toasty_core::driver::Capability::MSSQL).
//!
//! `mssql://` URLs are understood by [`toasty::Db::builder().connect()`] when the
//! `toasty` crate's `mssql` feature is enabled; alternatively construct the
//! driver and hand it to `build()`. The URL form is described by
//! [`MssqlConnectOptions`], which is the same configuration as a value:
//!
//! ```text
//! let driver = toasty_driver_mssql::Mssql::from_url(
//!     "mssql://sa:Password1!@localhost:1433/testdb?encrypt=on&trust_certificate=true",
//! )?;
//!
//! let db = toasty::Db::builder()
//!     .models(toasty::models!(User))
//!     .build(driver)
//!     .await?;
//! ```
//!
//! [`Mssql::new`] takes `mssql-tds`'s own connection configuration instead,
//! re-exported here as [`ClientContext`], so every option that client offers is
//! reachable through this driver without going through a URL:
//!
//! ```text
//! let mut context = toasty_driver_mssql::ClientContext::with_data_source("tcp:localhost,1433");
//! context.user_name = "sa".to_owned();
//! context.password = "Password1!".to_owned();
//! context.database = "testdb".to_owned();
//!
//! let driver = toasty_driver_mssql::Mssql::new(context);
//! ```

#![warn(missing_docs)]

mod connection;
mod driver;
mod migration;
pub mod options;
mod tds;

/// The `mssql-tds` client this driver speaks TDS with.
///
/// Re-exported because [`Mssql::new`] takes one of its types, and a caller
/// cannot otherwise name that type: the crate is a git dependency pinned to a
/// revision, so a caller who added it separately would get a different type
/// even if the revision matched.
pub use mssql_tds;
/// The connection configuration [`Mssql::new`] takes.
pub use mssql_tds::connection::client_context::ClientContext;
/// The encryption settings inside a [`ClientContext`], and the modes they take.
pub use mssql_tds::core::{EncryptionOptions, EncryptionSetting};

pub use driver::Mssql;
pub use options::MssqlConnectOptions;

/// The T-SQL scalar type name for a `datetimeoffset` column.
///
/// Shared with `toasty-driver-mssql-ext`, whose `MssqlDateTimeOffset` names
/// this type as its column storage.
#[doc(hidden)]
pub use tds::temporal::DATETIMEOFFSET;
