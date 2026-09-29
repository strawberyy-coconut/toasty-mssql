//! A model-facing wrapper for SQL Server's `datetimeoffset` column type.
//!
//! Toasty has no scalar for "an instant together with the offset it is displayed
//! in". `jiff::Timestamp` is a bare instant, and `jiff::Zoned` is documented to
//! store as text precisely because no column type carries an IANA zone name.
//! `datetimeoffset` *is* a column type that carries an offset, so the driver
//! supplies the matching Rust type, the same arrangement as the spatial
//! wrappers:
//!
//! ```
//! # use toasty_driver_mssql_ext::MssqlDateTimeOffset;
//! #[derive(toasty::Model)]
//! struct Event {
//!     #[key]
//!     id: i64,
//!
//!     happened_at: MssqlDateTimeOffset,
//! }
//! ```
//!
//! The column keeps the instant and the offset that applied at it. It has no
//! field for the IANA zone name, so a value read back carries a **fixed-offset**
//! zone rather than the one it was written with: `America/New_York` comes back as
//! `-04:00`. That is why this is a type of its own rather than a storage choice
//! for `jiff::Zoned` — it never claims to carry a zone, so nothing is silently
//! lost.
//!
//! `ORDER BY` and range filters over the column are chronological, because SQL
//! Server compares `datetimeoffset` by the instant rather than the displayed
//! offset.

use jiff::{
    Timestamp, Zoned,
    tz::{Offset, TimeZone},
};
use toasty::{
    Error, Result,
    schema::{Field, Load},
    stmt::{Assign, Assignment, Expr, IntoExpr, List, Path, set},
};
use toasty_core::{
    schema::{
        app::{FieldPrimitive, FieldTy},
        db,
    },
    stmt as core,
};

use toasty_driver_mssql::DATETIMEOFFSET;

/// An instant that SQL Server stores in a `datetimeoffset` column.
///
/// The wrapper holds a `jiff::Zoned`, whose zone is whatever the column carried
/// — a fixed offset, since that is all the column has a field for.
/// [`timestamp`](Self::timestamp) is the instant and [`offset`](Self::offset) is
/// the offset it is displayed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MssqlDateTimeOffset {
    value: Zoned,
}

impl MssqlDateTimeOffset {
    /// Wraps a zoned value, keeping its zone as given.
    pub fn from_zoned(value: Zoned) -> Self {
        Self { value }
    }

    /// Places an instant in a fixed offset, which is the shape a column round
    /// trips.
    pub fn from_timestamp(value: Timestamp, offset: Offset) -> Self {
        Self {
            value: value.to_zoned(TimeZone::fixed(offset)),
        }
    }

    /// The instant, independent of the offset it is displayed in.
    pub fn timestamp(&self) -> Timestamp {
        self.value.timestamp()
    }

    /// The offset the value is displayed in.
    pub fn offset(&self) -> Offset {
        self.value.offset()
    }

    /// The underlying zoned value.
    pub fn to_zoned(&self) -> &Zoned {
        &self.value
    }

    /// Consumes the wrapper, returning the underlying zoned value.
    pub fn into_zoned(self) -> Zoned {
        self.value
    }
}

impl From<MssqlDateTimeOffset> for core::Value {
    fn from(value: MssqlDateTimeOffset) -> Self {
        core::Value::Zoned(value.value)
    }
}

impl Load for MssqlDateTimeOffset {
    type Output = Self;

    fn ty() -> core::Type {
        core::Type::Zoned
    }

    fn load(value: core::Value) -> Result<Self> {
        match value {
            core::Value::Zoned(value) => Ok(Self { value }),
            other => Err(Error::type_conversion(other, "MssqlDateTimeOffset")),
        }
    }

    fn reload(target: &mut Self::Output, value: core::Value) -> Result<()> {
        *target = Self::load(value)?;
        Ok(())
    }
}

impl Field for MssqlDateTimeOffset {
    type ExprTarget = Self;
    type Path<Origin> = Path<Origin, Self>;
    type ListPath<Origin> = Path<Origin, List<Self::ExprTarget>>;
    type Update<'a> = ();
    type Inner = Self;

    /// The column type belongs to this type, not to the field, so a model never
    /// has to name it.
    fn field_ty(storage_ty: Option<db::Type>) -> FieldTy {
        FieldTy::Primitive(FieldPrimitive {
            ty: <Self as Load>::ty(),
            storage_ty: storage_ty.or_else(|| Some(db::Type::Custom(DATETIMEOFFSET.to_owned()))),
            serialize: None,
        })
    }

    fn new_path<Origin>(path: Path<Origin, Self>) -> Self::Path<Origin> {
        path
    }

    fn new_list_path<Origin>(path: Path<Origin, List<Self::ExprTarget>>) -> Self::ListPath<Origin> {
        path
    }

    fn new_update<'a>(
        _assignments: &'a mut core::Assignments,
        _projection: core::Projection,
    ) -> Self::Update<'a> {
    }

    fn key_constraint<Origin>(&self, target: Path<Origin, Self::Inner>) -> Expr<bool> {
        target.eq(self.clone())
    }
}

impl IntoExpr<Self> for MssqlDateTimeOffset {
    fn into_expr(self) -> Expr<Self> {
        Expr::from_untyped(core::Expr::Value(self.into()))
    }

    fn by_ref(&self) -> Expr<Self> {
        Expr::from_untyped(core::Expr::Value(self.clone().into()))
    }
}

impl Assign<MssqlDateTimeOffset> for MssqlDateTimeOffset {
    fn into_assignment(self) -> Assignment<MssqlDateTimeOffset> {
        set(self.into_expr())
    }
}
