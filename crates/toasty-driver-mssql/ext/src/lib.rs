//! Model-facing SQL Server extensions for [`toasty-driver-mssql`].
//!
//! The driver itself speaks `toasty_core` alone. This crate adds the pieces
//! that need the `toasty` crate — SQL Server's built-in functions, the
//! `geometry` / `geography` column types, and the `datetimeoffset` column type
//! — as optional features.
//!
//! Neither [`toasty-driver-mssql`] nor `toasty` depends on this crate, so
//! adding it cannot create a dependency cycle; a model that wants any of these
//! adds it alongside the driver:
//!
//! ```toml
//! [dependencies]
//! toasty = "0.11"
//! toasty-driver-mssql = "0.11"
//! toasty-driver-mssql-ext = { version = "0.11", features = ["spatial", "funcs"] }
//! ```
//!
//! # Features
//!
//! * `spatial` — [`MssqlGeometry`] / [`MssqlGeography`], and the methods that
//!   operate on them.
//! * `datetimeoffset` — [`MssqlDateTimeOffset`], which stores an instant with
//!   the offset it is displayed in a `datetimeoffset` column.
//! * `funcs-json`, `funcs-string`, `funcs-math`, `funcs-date` — SQL Server's
//!   built-in functions as extension traits, one Cargo feature per Microsoft
//!   [category][cats]; `funcs` turns on all four. The [`funcs`] module documents
//!   how a call is carried through an AST that has no node for one.
//!
//! [cats]: https://learn.microsoft.com/en-us/sql/t-sql/functions/functions
//! [`funcs`]: https://docs.rs/toasty-driver-mssql-ext/latest/toasty_driver_mssql_ext/#reexports

#![warn(missing_docs)]

#[cfg(feature = "datetimeoffset")]
pub mod datetime_offset;

#[cfg(feature = "toasty")]
mod funcs;

#[cfg(feature = "spatial")]
pub mod spatial;

#[cfg(feature = "datetimeoffset")]
pub use datetime_offset::MssqlDateTimeOffset;

#[cfg(feature = "funcs-date")]
pub use funcs::DatePart;
#[cfg(feature = "funcs-date")]
pub use funcs::MssqlDate;
#[cfg(feature = "funcs-json")]
pub use funcs::MssqlJson;
#[cfg(feature = "funcs-math")]
pub use funcs::MssqlMath;
#[cfg(feature = "funcs-string")]
pub use funcs::MssqlStr;
#[cfg(feature = "toasty")]
pub use funcs::{MssqlLiteral, lit};

#[cfg(feature = "spatial")]
pub use spatial::{
    MssqlGeography, MssqlGeographyExt, MssqlGeometry, MssqlGeometryExt, MssqlSpatial,
};
