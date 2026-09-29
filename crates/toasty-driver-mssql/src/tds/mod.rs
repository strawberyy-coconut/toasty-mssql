//! The TDS layer: connection establishment, statement execution and result
//! reading on top of `mssql-tds`.
//!
//! Toasty's [`Connection`](toasty_core::driver::Connection) trait is
//! request/response: `exec` is handed an [`Operation`](toasty_core::driver::Operation)
//! and returns an owned [`ExecResponse`](toasty_core::driver::ExecResponse). The
//! `mssql-tds` client, by contrast, hands out a borrowed cursor that must be
//! drained before anything else is sent. This module bridges the two by always
//! draining the result set inside the call and returning owned rows, which keeps
//! the connection idle and ready for the next operation.

pub(crate) mod client;
pub(crate) mod decode;
pub(crate) mod params;
pub(crate) mod temporal;

pub(crate) use client::{Client, ExecOutcome, ResultSet};
