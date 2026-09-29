//! Model-facing types for SQL Server's spatial columns.
//!
//! Toasty has no spatial type, but `toasty::schema::Field` is public and
//! unsealed, so a driver can add a scalar of its own. That is what these are:
//! each reports `stmt::Type::Bytes`, because SQL Server sends a spatial value as
//! `varbinary` holding its own serialization ([MS-SSCLRT]) rather than WKB, and
//! each supplies the `geometry` / `geography` column type itself, so a model
//! just names the type:
//!
//! ```
//! # use toasty_driver_mssql_ext::{MssqlGeography, MssqlGeometry};
//! #[derive(toasty::Model)]
//! struct Place {
//!     #[key]
//!     id: i64,
//!
//!     shape: MssqlGeometry,
//!     area: MssqlGeography,
//! }
//! ```
//!
//! The value is the serialized payload, so it can be written straight back. For
//! a shape computed in Rust, [`MssqlGeometry::from_geometry`] encodes one and
//! [`MssqlGeometry::to_geometry`] decodes it again; both go through `codec`,
//! which implements SQL Server's own serialization.
//!
//! [MS-SSCLRT]: https://learn.microsoft.com/en-us/openspecs/sql_server_protocols/ms-ssclrt/

mod codec;
mod funcs;

pub use funcs::{MssqlGeographyExt, MssqlGeometryExt, MssqlSpatial};

use geo_types::Geometry;
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

macro_rules! spatial_type {
    (
        $(#[$meta:meta])*
        $name:ident,
        $column:literal
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            bytes: Vec<u8>,
        }

        impl $name {
            /// Wraps a value already in SQL Server's serialized form.
            ///
            /// This is the form a column returns, so the usual way to get one is
            /// to read a row. SQL Server validates the payload when it is
            /// written, so an invalid one fails on write rather than silently
            /// round-tripping.
            pub fn from_bytes(bytes: Vec<u8>) -> Self {
                Self { bytes }
            }

            /// The serialized form, exactly as the server stores and sends it.
            pub fn as_bytes(&self) -> &[u8] {
                &self.bytes
            }

            /// The spatial reference id carried in the payload.
            ///
            /// The SRID is the first four bytes, little endian, of both a
            /// `geometry` and a `geography` payload. Returns `None` if the
            /// payload is too short to hold one.
            pub fn srid(&self) -> Option<i32> {
                let bytes: [u8; 4] = self.bytes.get(..4)?.try_into().ok()?;
                Some(i32::from_le_bytes(bytes))
            }

            /// Encodes a `geo-types` geometry with the given SRID.
            ///
            /// Coordinates are `(x, y)`. For a `geography` that means `x` is
            /// longitude and `y` is latitude, the opposite order to the
            /// `(lat long)` SQL Server's own geography WKT uses: a
            /// `Coord { x: 1.0, y: 2.0 }` written here reads back — in the
            /// server's terms — as `LINESTRING (2 1, …)`. The bytes are still
            /// exactly what the server produces for that shape.
            pub fn from_geometry(geometry: Geometry<f64>, srid: i32) -> Self {
                Self {
                    bytes: codec::encode(srid, &geometry),
                }
            }

            /// Decodes the payload back into a `geo-types` geometry.
            ///
            /// Fails rather than guessing when the payload holds something
            /// `geo-types` cannot represent: a version 2 (curved) value, one
            /// carrying Z or M coordinates, or an empty point. The coordinates
            /// come back as they were written: see
            /// [`from_geometry`](Self::from_geometry) for what `x` and `y`
            /// mean.
            pub fn to_geometry(&self) -> Result<Geometry<f64>> {
                Ok(codec::decode(&self.bytes)?.1)
            }
        }

        impl From<$name> for core::Value {
            fn from(value: $name) -> Self {
                core::Value::Bytes(value.bytes)
            }
        }

        impl Load for $name {
            type Output = Self;

            fn ty() -> core::Type {
                core::Type::Bytes
            }

            fn load(value: core::Value) -> Result<Self> {
                match value {
                    core::Value::Bytes(bytes) => Ok(Self { bytes }),
                    other => Err(Error::type_conversion(other, stringify!($name))),
                }
            }

            fn reload(target: &mut Self::Output, value: core::Value) -> Result<()> {
                *target = Self::load(value)?;
                Ok(())
            }
        }

        impl Field for $name {
            type ExprTarget = Self;
            type Path<Origin> = Path<Origin, Self>;
            type ListPath<Origin> = Path<Origin, List<Self::ExprTarget>>;
            type Update<'a> = ();
            type Inner = Self;

            /// The column type belongs to this type, not to the field, so a
            /// model never has to repeat it.
            fn field_ty(storage_ty: Option<db::Type>) -> FieldTy {
                FieldTy::Primitive(FieldPrimitive {
                    ty: <Self as Load>::ty(),
                    storage_ty: storage_ty.or_else(|| Some(db::Type::Custom($column.to_owned()))),
                    serialize: None,
                })
            }

            fn new_path<Origin>(path: Path<Origin, Self>) -> Self::Path<Origin> {
                path
            }

            fn new_list_path<Origin>(
                path: Path<Origin, List<Self::ExprTarget>>,
            ) -> Self::ListPath<Origin> {
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

        impl IntoExpr<Self> for $name {
            fn into_expr(self) -> Expr<Self> {
                Expr::from_untyped(core::Expr::Value(self.into()))
            }

            fn by_ref(&self) -> Expr<Self> {
                Expr::from_untyped(core::Expr::Value(self.clone().into()))
            }
        }

        impl Assign<$name> for $name {
            fn into_assignment(self) -> Assignment<$name> {
                set(self.into_expr())
            }
        }
    };
}

spatial_type! {
    /// A SQL Server `geometry` value and its spatial reference id.
    ///
    /// Planar: SQL Server does not interpret the coordinates, and an SRID of
    /// `0` — the default — is allowed, so a `geometry` may carry no meaningful
    /// reference system at all.
    MssqlGeometry,
    "geometry"
}

spatial_type! {
    /// A SQL Server `geography` value and its spatial reference id.
    ///
    /// Round-earth: coordinates are latitude and longitude on an ellipsoid, and
    /// SQL Server requires an SRID it knows (such as `4326`); it rejects `0`.
    /// Lengths and areas are computed in metres and square metres.
    MssqlGeography,
    "geography"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload of `geometry::STGeomFromText('POINT(1 2)', 4326)`, as SQL
    /// Server sends it: SRID, then the version 1 single-point encoding.
    const POINT: [u8; 22] = [
        230, 16, 0, 0, // SRID 4326, little endian
        1, 12, // version 1, little endian, valid, single point
        0, 0, 0, 0, 0, 0, 240, 63, // x = 1.0
        0, 0, 0, 0, 0, 0, 0, 64, // y = 2.0
    ];

    #[test]
    fn reads_the_srid_from_the_header() {
        let value = MssqlGeometry::from_bytes(POINT.to_vec());

        assert_eq!(value.srid(), Some(4326));
        assert_eq!(value.as_bytes(), POINT);
    }

    #[test]
    fn reports_no_srid_for_a_short_payload() {
        let value = MssqlGeography::from_bytes(vec![1, 2, 3]);

        assert_eq!(value.srid(), None);
    }

    #[test]
    fn loads_from_and_converts_back_to_bytes() {
        let value = MssqlGeometry::load(core::Value::Bytes(POINT.to_vec())).unwrap();

        assert_eq!(value, MssqlGeometry::from_bytes(POINT.to_vec()));
        assert_eq!(core::Value::from(value), core::Value::Bytes(POINT.to_vec()));
    }

    #[test]
    fn rejects_a_non_binary_value() {
        let error = MssqlGeometry::load(core::Value::from(1_i64)).unwrap_err();

        assert!(error.to_string().contains("MssqlGeometry"), "got: {error}");
    }

    #[test]
    fn supplies_its_own_column_type() {
        assert_eq!(MssqlGeometry::ty(), core::Type::Bytes);

        // A model that names no column type still gets a spatial column.
        assert_eq!(storage_ty::<MssqlGeometry>(None), custom("geometry"));
        assert_eq!(storage_ty::<MssqlGeography>(None), custom("geography"));

        // An explicit column type wins.
        assert_eq!(
            storage_ty::<MssqlGeometry>(Some(db::Type::Text)),
            Some(db::Type::Text)
        );
    }

    fn custom(name: &str) -> Option<db::Type> {
        Some(db::Type::Custom(name.to_owned()))
    }

    fn storage_ty<T: Field>(storage_ty: Option<db::Type>) -> Option<db::Type> {
        let FieldTy::Primitive(primitive) = T::field_ty(storage_ty) else {
            panic!("a spatial field is a primitive");
        };

        assert_eq!(primitive.ty, core::Type::Bytes);
        primitive.storage_ty
    }
}
