//! Spatial methods, for the types in [`super`].
//!
//! These are **methods**, not functions: T-SQL has no `STArea(col)`, only
//! `col.STArea()`. That is why they are built with the method form of the call
//! carrier (see [`crate::funcs`]) rather than a plain `call`.
//!
//! Every method is usable on a column path *and* on the result of another call,
//! which is what makes the ones that answer a shape worth having — a geometry
//! cannot be compared, so `st_union(…).st_area()` is the only way to ask about
//! it:
//!
//! ```ignore
//! use toasty_driver_mssql_ext::{MssqlSpatial as _, lit};
//!
//! Widget::filter(Widget::fields().shape().st_intersects("POINT(0.5 0.5)", 0))
//!
//! // The union's area, not the column's.
//! Widget::filter(
//!     Widget::fields().shape().st_union("POLYGON((…))", 0).st_area().gt(lit(30.0)),
//! )
//! ```
//!
//! A method that takes another geometry takes it as **well-known text**, which
//! this module wraps in the right constructor for the column's own type:
//! `geometry::STGeomFromText(…)` for a `geometry` column and
//! `geography::STGeomFromText(…)` for a `geography` one. Note the coordinate
//! order for each — see [`MssqlSpatial::st_distance`].
use toasty::stmt::{Expr, Path};
use toasty_core::stmt;

use crate::funcs::{Operand, method, method_check, text};

use super::{MssqlGeography, MssqlGeometry};

/// Spatial methods common to `geometry` and `geography`.
pub trait MssqlSpatial {
    /// The Rust type of a shape this column holds, for the methods that answer
    /// one.
    type Shape;

    /// The expression a call applies to: the column, or the call before it.
    #[doc(hidden)]
    fn operand(&self) -> stmt::Expr;

    /// The constructor keyword for the column's own type — `geometry` or
    /// `geography` — so an argument is built as the same type as the receiver.
    #[doc(hidden)]
    fn constructor() -> &'static str;

    // -- Predicates, each taking another shape -------------------------------

    /// `col.STIntersects(other)` — `1` when the two shapes share any point.
    fn st_intersects(&self, wkt: &str, srid: i32) -> Expr<bool> {
        method_check(
            &self.operand(),
            "STIntersects",
            vec![other(Self::constructor(), wkt, srid)],
        )
    }

    /// `col.STContains(other)` — `1` when `other` lies entirely within `col`.
    ///
    /// Not symmetric, and not reflexive: a value does not contain itself.
    fn st_contains(&self, wkt: &str, srid: i32) -> Expr<bool> {
        method_check(
            &self.operand(),
            "STContains",
            vec![other(Self::constructor(), wkt, srid)],
        )
    }

    /// `col.STWithin(other)` — the inverse of [`st_contains`](Self::st_contains).
    fn st_within(&self, wkt: &str, srid: i32) -> Expr<bool> {
        method_check(
            &self.operand(),
            "STWithin",
            vec![other(Self::constructor(), wkt, srid)],
        )
    }

    /// `col.STEquals(other)` — `1` when the two cover the same point set.
    ///
    /// Compares the *shape*, not the text: two different WKT spellings of the
    /// same region are equal.
    fn st_equals(&self, wkt: &str, srid: i32) -> Expr<bool> {
        method_check(
            &self.operand(),
            "STEquals",
            vec![other(Self::constructor(), wkt, srid)],
        )
    }

    /// `col.STDisjoint(other)` — `1` when the two share no point at all.
    fn st_disjoint(&self, wkt: &str, srid: i32) -> Expr<bool> {
        method_check(
            &self.operand(),
            "STDisjoint",
            vec![other(Self::constructor(), wkt, srid)],
        )
    }

    /// `col.STOverlaps(other)` — `1` when they share points but neither
    /// contains the other.
    fn st_overlaps(&self, wkt: &str, srid: i32) -> Expr<bool> {
        method_check(
            &self.operand(),
            "STOverlaps",
            vec![other(Self::constructor(), wkt, srid)],
        )
    }

    // -- Predicates taking nothing --------------------------------------------

    /// `col.STIsValid()` — `1` when the instance is well formed.
    ///
    /// "Accepted" and "valid" are different: SQL Server will store a polygon
    /// whose rings cross, and it is that crossing this rejects. See
    /// [`st_make_valid`](MssqlGeometryExt::st_make_valid) for the repair.
    fn st_is_valid(&self) -> Expr<bool> {
        method_check(&self.operand(), "STIsValid", vec![])
    }

    /// `col.STIsEmpty()` — `1` for the empty geometry.
    fn st_is_empty(&self) -> Expr<bool> {
        method_check(&self.operand(), "STIsEmpty", vec![])
    }

    // -- Measurements ---------------------------------------------------------

    /// `col.STArea()` — square units, which for `geography` means square metres
    /// on the ellipsoid.
    fn st_area(&self) -> Expr<f64> {
        method(&self.operand(), stmt::Type::F64, "STArea", vec![])
    }

    /// `col.STLength()` — the perimeter of a polygon, or the length of a line.
    ///
    /// Ring lengths are **summed**, unlike areas which are signed: a hole
    /// subtracts from `STArea` and adds to `STLength`.
    fn st_length(&self) -> Expr<f64> {
        method(&self.operand(), stmt::Type::F64, "STLength", vec![])
    }

    /// `col.STNumPoints()` — the number of points in the value.
    fn st_num_points(&self) -> Expr<i32> {
        method(&self.operand(), stmt::Type::I32, "STNumPoints", vec![])
    }

    /// `col.STDistance(other)` — the shortest distance between the two values.
    ///
    /// `wkt` is well-known text and `srid` its spatial reference id, which for
    /// `geography` must be one SQL Server knows (such as `4326`) and must match
    /// the column's.
    ///
    /// **The coordinate order differs between the two types.** A `geometry`
    /// reads `wkt` as `(x y)`. A `geography` reads it as `(lat long)` — the
    /// opposite order — which is the same trap documented on the codec, and the
    /// reason the two are separate Rust types rather than one with a parameter.
    fn st_distance(&self, wkt: &str, srid: i32) -> Expr<f64> {
        method(
            &self.operand(),
            stmt::Type::F64,
            "STDistance",
            vec![other(Self::constructor(), wkt, srid)],
        )
    }

    // -- Descriptions ---------------------------------------------------------

    /// `col.STGeometryType()` — `Point`, `LineString`, `Polygon`, and so on.
    fn st_geometry_type(&self) -> Expr<String> {
        method(
            &self.operand(),
            stmt::Type::String,
            "STGeometryType",
            vec![],
        )
    }

    /// `col.STAsText()` — the value's well-known text.
    fn st_as_text(&self) -> Expr<String> {
        method(&self.operand(), stmt::Type::String, "STAsText", vec![])
    }

    /// `col.ToString()` — the same rendering as
    /// [`st_as_text`](Self::st_as_text), spelled as the method T-SQL documents
    /// for debugging.
    fn to_string(&self) -> Expr<String> {
        method(&self.operand(), stmt::Type::String, "ToString", vec![])
    }

    // -- Shapes ---------------------------------------------------------------

    /// `col.STUnion(other)` — everything in either shape.
    ///
    /// Answers a shape, so it needs another call to say anything about it:
    /// `st_union(…).st_area()`.
    fn st_union(&self, wkt: &str, srid: i32) -> Expr<Self::Shape> {
        method(
            &self.operand(),
            stmt::Type::Bytes,
            "STUnion",
            vec![other(Self::constructor(), wkt, srid)],
        )
    }

    /// `col.STIntersection(other)` — the points in both.
    fn st_intersection(&self, wkt: &str, srid: i32) -> Expr<Self::Shape> {
        method(
            &self.operand(),
            stmt::Type::Bytes,
            "STIntersection",
            vec![other(Self::constructor(), wkt, srid)],
        )
    }

    /// `col.STDifference(other)` — the points in `col` that are not in `other`.
    fn st_difference(&self, wkt: &str, srid: i32) -> Expr<Self::Shape> {
        method(
            &self.operand(),
            stmt::Type::Bytes,
            "STDifference",
            vec![other(Self::constructor(), wkt, srid)],
        )
    }

    /// `col.STSymDifference(other)` — the points in exactly one of the two.
    fn st_sym_difference(&self, wkt: &str, srid: i32) -> Expr<Self::Shape> {
        method(
            &self.operand(),
            stmt::Type::Bytes,
            "STSymDifference",
            vec![other(Self::constructor(), wkt, srid)],
        )
    }

    /// `col.STBuffer(distance)` — the shape grown by `distance`.
    ///
    /// On a `geography` column this answers NULL, not an error, when the result
    /// would exceed a hemisphere — as do the four set operations above.
    fn st_buffer(&self, distance: f64) -> Expr<Self::Shape> {
        method(
            &self.operand(),
            stmt::Type::Bytes,
            "STBuffer",
            vec![distance.to_string()],
        )
    }
}

/// Spatial methods only a `geometry` column has.
///
/// `geography` offers a reduced OGC surface: no `STTouches`, `STCrosses` or
/// `STIsSimple` (the server rejects each with "Could not find method … for type
/// SqlGeography"), and its envelope is spelled
/// `EnvelopeCenter`/`EnvelopeAngle` rather than `STEnvelope`. Its values are
/// always valid, so it has no `MakeValid` either.
pub trait MssqlGeometryExt {
    /// The expression a call applies to: the column, or the call before it.
    #[doc(hidden)]
    fn operand(&self) -> stmt::Expr;

    /// `col.STTouches(other)` — `1` when they meet only at their boundaries.
    fn st_touches(&self, wkt: &str, srid: i32) -> Expr<bool> {
        method_check(
            &self.operand(),
            "STTouches",
            vec![other("geometry", wkt, srid)],
        )
    }

    /// `col.STCrosses(other)` — `1` when they cross in a lower dimension than
    /// either of them.
    fn st_crosses(&self, wkt: &str, srid: i32) -> Expr<bool> {
        method_check(
            &self.operand(),
            "STCrosses",
            vec![other("geometry", wkt, srid)],
        )
    }

    /// `col.STIsSimple()` — `1` when the instance does not intersect itself.
    fn st_is_simple(&self) -> Expr<bool> {
        method_check(&self.operand(), "STIsSimple", vec![])
    }

    /// `col.STEnvelope()` — the bounding box, as a polygon.
    fn st_envelope(&self) -> Expr<MssqlGeometry> {
        method(&self.operand(), stmt::Type::Bytes, "STEnvelope", vec![])
    }

    /// `col.STConvexHull()` — the smallest convex shape containing the value.
    fn st_convex_hull(&self) -> Expr<MssqlGeometry> {
        method(&self.operand(), stmt::Type::Bytes, "STConvexHull", vec![])
    }

    /// `col.STBoundary()` — the boundary, in a lower dimension than the value.
    fn st_boundary(&self) -> Expr<MssqlGeometry> {
        method(&self.operand(), stmt::Type::Bytes, "STBoundary", vec![])
    }

    /// `col.STCentroid()` — the geometric centre.
    fn st_centroid(&self) -> Expr<MssqlGeometry> {
        method(&self.operand(), stmt::Type::Bytes, "STCentroid", vec![])
    }

    /// `col.STPointOnSurface()` — some point guaranteed to be on the value.
    ///
    /// Unlike the centroid, which can fall outside a concave shape.
    fn st_point_on_surface(&self) -> Expr<MssqlGeometry> {
        method(
            &self.operand(),
            stmt::Type::Bytes,
            "STPointOnSurface",
            vec![],
        )
    }

    /// `col.MakeValid()` — a valid instance close to an invalid one.
    ///
    /// **This can change the shape's *type*.** An invalid `Polygon` whose rings
    /// overlap comes back as a `MultiPolygon`, and one whose points all coincide
    /// comes back as a `Point`. The Rust type tag is therefore a claim only
    /// about the *storage* type, not about which instance type is answered.
    fn st_make_valid(&self) -> Expr<MssqlGeometry> {
        method(&self.operand(), stmt::Type::Bytes, "MakeValid", vec![])
    }
}

/// Spatial methods only a `geography` column has.
pub trait MssqlGeographyExt {
    /// The expression a call applies to: the column, or the call before it.
    #[doc(hidden)]
    fn operand(&self) -> stmt::Expr;

    /// `col.ReorientObject()` — the same ring with its orientation reversed.
    ///
    /// `geography` reads a polygon's interior by the left-hand rule, so a ring
    /// given the wrong way round describes the *rest of the globe* rather than
    /// its own interior. This is the repair.
    fn st_reorient_object(&self) -> Expr<MssqlGeography> {
        method(&self.operand(), stmt::Type::Bytes, "ReorientObject", vec![])
    }
}

macro_rules! mssql_spatial {
    ($($ty:ty => $ctor:literal;)*) => {
        $(
            impl<Origin> MssqlSpatial for Path<Origin, $ty> {
                type Shape = $ty;

                fn operand(&self) -> stmt::Expr {
                    Operand::operand(self)
                }

                fn constructor() -> &'static str {
                    $ctor
                }
            }

            impl MssqlSpatial for Expr<$ty> {
                type Shape = $ty;

                fn operand(&self) -> stmt::Expr {
                    Operand::operand(self)
                }

                fn constructor() -> &'static str {
                    $ctor
                }
            }
        )*
    };
}

mssql_spatial! {
    MssqlGeometry => "geometry";
    MssqlGeography => "geography";
}

impl<Origin> MssqlGeometryExt for Path<Origin, MssqlGeometry> {
    fn operand(&self) -> stmt::Expr {
        Operand::operand(self)
    }
}

impl MssqlGeometryExt for Expr<MssqlGeometry> {
    fn operand(&self) -> stmt::Expr {
        Operand::operand(self)
    }
}

impl<Origin> MssqlGeographyExt for Path<Origin, MssqlGeography> {
    fn operand(&self) -> stmt::Expr {
        Operand::operand(self)
    }
}

impl MssqlGeographyExt for Expr<MssqlGeography> {
    fn operand(&self) -> stmt::Expr {
        Operand::operand(self)
    }
}

/// `geometry::STGeomFromText(N'POINT(1 2)', 4326)` — the constructor for the
/// column's own type, so a geography column gets a geography argument.
fn other(constructor: &str, wkt: &str, srid: i32) -> String {
    format!("{constructor}::STGeomFromText({}, {srid})", text(wkt))
}
