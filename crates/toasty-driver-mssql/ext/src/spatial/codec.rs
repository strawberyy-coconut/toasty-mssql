//! SQL Server's own serialization of a spatial value, documented as [MS-SSCLRT].
//!
//! This is **not** WKB, and reading one as the other silently misinterprets it.
//! The payload carries its SRID in the first four bytes, which WKB has no place
//! for.
//!
//! Only version 1, two-dimensional, non-curved values are handled: `geo-types`
//! coordinates are two-dimensional and the crate has no representation for
//! curves. Empty shapes round-trip, because SQL Server encodes them as a shape
//! with no figures.
//!
//! Adapted from the Apache-2.0 licensed `sqlx-mssql-rs` driver that ships in
//! `old-sqlx/` (its `types/geo.rs`), including the payloads its tests were
//! captured with.
//!
//! [MS-SSCLRT]: https://learn.microsoft.com/en-us/openspecs/sql_server_protocols/ms-ssclrt/

use geo_types::{
    Coord, Geometry, GeometryCollection, LineString, MultiLineString, MultiPoint, MultiPolygon,
    Point, Polygon,
};
use toasty_core::{Error, Result};

/// Serialization properties: the value carries Z coordinates.
const PROP_Z: u8 = 0x01;
/// Serialization properties: the value carries M (measure) coordinates.
const PROP_M: u8 = 0x02;
/// Serialization properties: the value is valid (always set by SQL Server).
const PROP_VALID: u8 = 0x04;
/// Serialization properties: the value is a single point, whose counts are
/// omitted.
const PROP_SINGLE_POINT: u8 = 0x08;
/// Serialization properties: the value is a single line segment, whose counts
/// are omitted.
const PROP_SINGLE_LINE: u8 = 0x10;

/// Version 1 figure attribute: an interior polygon ring.
const FIGURE_INTERIOR_RING: u8 = 0x00;
/// Version 1 figure attribute: a point or a line.
const FIGURE_STROKE: u8 = 0x01;
/// Version 1 figure attribute: an exterior polygon ring.
const FIGURE_EXTERIOR_RING: u8 = 0x02;

const SHAPE_POINT: u8 = 1;
const SHAPE_LINESTRING: u8 = 2;
const SHAPE_POLYGON: u8 = 3;
const SHAPE_MULTIPOINT: u8 = 4;
const SHAPE_MULTILINESTRING: u8 = 5;
const SHAPE_MULTIPOLYGON: u8 = 6;
const SHAPE_GEOMETRYCOLLECTION: u8 = 7;
const SHAPE_CIRCULARSTRING: u8 = 8;
const SHAPE_COMPOUNDCURVE: u8 = 9;
const SHAPE_CURVEPOLYGON: u8 = 10;
const SHAPE_FULLGLOBE: u8 = 11;

/// A `FIGURE` structure: a run of points within one shape.
struct Figure {
    attribute: u8,
    point_offset: u32,
}

/// A `SHAPE` structure: one OGC simple feature.
struct Shape {
    parent: i32,
    figure_offset: i32,
    shape_type: u8,
}

/// Decodes a spatial payload into its SRID and geometry.
pub(super) fn decode(bytes: &[u8]) -> Result<(i32, Geometry<f64>)> {
    let mut reader = Reader::new(bytes);

    let srid = reader.i32()?;
    let version = reader.u8()?;

    if version != 1 {
        return Err(Error::unsupported_feature(format!(
            "SQL Server sent a spatial serialization version {version}; \
             only version 1 (planar, two-dimensional, non-curved) is supported"
        )));
    }

    let properties = reader.u8()?;

    if properties & (PROP_Z | PROP_M) != 0 {
        return Err(Error::unsupported_feature(
            "SQL Server sent a three-dimensional or measured spatial value, \
             which has no `geo-types` representation",
        ));
    }

    let point_count = if properties & PROP_SINGLE_POINT != 0 {
        1
    } else if properties & PROP_SINGLE_LINE != 0 {
        2
    } else {
        reader.u32()? as usize
    };

    let mut points = Vec::with_capacity(point_count);
    for _ in 0..point_count {
        points.push(Coord {
            x: reader.f64()?,
            y: reader.f64()?,
        });
    }

    let (figures, shapes) = if properties & (PROP_SINGLE_POINT | PROP_SINGLE_LINE) != 0 {
        let shape_type = if properties & PROP_SINGLE_POINT != 0 {
            SHAPE_POINT
        } else {
            SHAPE_LINESTRING
        };

        (
            vec![Figure {
                attribute: FIGURE_STROKE,
                point_offset: 0,
            }],
            vec![Shape {
                parent: -1,
                figure_offset: 0,
                shape_type,
            }],
        )
    } else {
        let figure_count = reader.u32()? as usize;
        let mut figures = Vec::with_capacity(figure_count);
        for _ in 0..figure_count {
            figures.push(Figure {
                attribute: reader.u8()?,
                point_offset: reader.u32()?,
            });
        }

        let shape_count = reader.u32()? as usize;
        let mut shapes = Vec::with_capacity(shape_count);
        for _ in 0..shape_count {
            shapes.push(Shape {
                parent: reader.i32()?,
                figure_offset: reader.i32()?,
                shape_type: reader.u8()?,
            });
        }

        (figures, shapes)
    };

    let top = shapes
        .iter()
        .position(|shape| shape.parent == -1)
        .ok_or_else(|| Error::invalid_result("spatial payload has no top-level shape"))?;

    Ok((srid, build_shape(top, &shapes, &figures, &points)?))
}

/// Encodes a geometry with the given SRID as a version 1 payload.
pub(super) fn encode(srid: i32, geometry: &Geometry<f64>) -> Vec<u8> {
    let mut points = Vec::new();
    let mut figures = Vec::new();
    let mut shapes = Vec::new();

    encode_geometry(geometry, -1, &mut points, &mut figures, &mut shapes);

    let mut bytes =
        Vec::with_capacity(8 + points.len() * 16 + figures.len() * 5 + shapes.len() * 9);
    bytes.extend_from_slice(&srid.to_le_bytes());
    bytes.push(1);
    bytes.push(PROP_VALID);
    bytes.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for point in &points {
        bytes.extend_from_slice(&point.x.to_le_bytes());
        bytes.extend_from_slice(&point.y.to_le_bytes());
    }
    bytes.extend_from_slice(&(figures.len() as u32).to_le_bytes());
    for figure in &figures {
        bytes.push(figure.attribute);
        bytes.extend_from_slice(&figure.point_offset.to_le_bytes());
    }
    bytes.extend_from_slice(&(shapes.len() as u32).to_le_bytes());
    for shape in &shapes {
        bytes.extend_from_slice(&shape.parent.to_le_bytes());
        bytes.extend_from_slice(&shape.figure_offset.to_le_bytes());
        bytes.push(shape.shape_type);
    }

    bytes
}

/// Rebuilds one shape, recursing into the child shapes that reference it.
fn build_shape(
    index: usize,
    shapes: &[Shape],
    figures: &[Figure],
    points: &[Coord<f64>],
) -> Result<Geometry<f64>> {
    let shape = &shapes[index];
    let children = || -> Vec<usize> {
        (0..shapes.len())
            .filter(|candidate| shapes[*candidate].parent == index as i32)
            .collect()
    };

    match shape.shape_type {
        SHAPE_POINT => {
            let coord = first_coord(shape.figure_offset, figures, points)?;
            Ok(Geometry::Point(Point::new(coord.x, coord.y)))
        }
        SHAPE_LINESTRING => Ok(Geometry::LineString(LineString::new(figure_coords(
            shape.figure_offset,
            figures,
            points,
        )?))),
        SHAPE_POLYGON => Ok(Geometry::Polygon(build_polygon(
            shape.figure_offset,
            figures,
            points,
        )?)),
        SHAPE_MULTIPOINT => {
            let mut result = Vec::new();
            for child in children() {
                let coord = first_coord(shapes[child].figure_offset, figures, points)?;
                result.push(Point::new(coord.x, coord.y));
            }
            Ok(Geometry::MultiPoint(MultiPoint::new(result)))
        }
        SHAPE_MULTILINESTRING => {
            let mut result = Vec::new();
            for child in children() {
                result.push(LineString::new(figure_coords(
                    shapes[child].figure_offset,
                    figures,
                    points,
                )?));
            }
            Ok(Geometry::MultiLineString(MultiLineString::new(result)))
        }
        SHAPE_MULTIPOLYGON => {
            let mut result = Vec::new();
            for child in children() {
                result.push(build_polygon(shapes[child].figure_offset, figures, points)?);
            }
            Ok(Geometry::MultiPolygon(MultiPolygon::new(result)))
        }
        SHAPE_GEOMETRYCOLLECTION => {
            let mut result = Vec::new();
            for child in children() {
                result.push(build_shape(child, shapes, figures, points)?);
            }
            Ok(Geometry::GeometryCollection(GeometryCollection::new_from(
                result,
            )))
        }
        SHAPE_CIRCULARSTRING | SHAPE_COMPOUNDCURVE | SHAPE_CURVEPOLYGON | SHAPE_FULLGLOBE => {
            Err(Error::unsupported_feature(format!(
                "SQL Server sent a spatial shape type {} (curved or globe geometry), \
                 which has no `geo-types` representation",
                shape.shape_type
            )))
        }
        other => Err(Error::invalid_result(format!(
            "unknown SQL Server spatial shape type {other}"
        ))),
    }
}

/// Builds a polygon from its exterior figure and the interior figures that
/// follow it.
fn build_polygon(
    figure_offset: i32,
    figures: &[Figure],
    points: &[Coord<f64>],
) -> Result<Polygon<f64>> {
    if figure_offset < 0 {
        return Ok(Polygon::new(LineString::new(Vec::new()), Vec::new()));
    }

    let exterior = LineString::new(figure_coords(figure_offset, figures, points)?);
    let mut interiors = Vec::new();

    // Interior rings follow their exterior ring until the next exterior ring,
    // which begins the next polygon in a multi-polygon.
    let start = usize::try_from(figure_offset).unwrap_or(usize::MAX);
    for (index, figure) in figures.iter().enumerate().skip(start + 1) {
        if figure.attribute == FIGURE_EXTERIOR_RING {
            break;
        }
        if figure.attribute == FIGURE_INTERIOR_RING {
            interiors.push(LineString::new(figure_coords(
                index as i32,
                figures,
                points,
            )?));
        }
    }

    Ok(Polygon::new(exterior, interiors))
}

/// Returns the first coordinate of a figure, failing on an empty figure.
fn first_coord(
    figure_offset: i32,
    figures: &[Figure],
    points: &[Coord<f64>],
) -> Result<Coord<f64>> {
    figure_coords(figure_offset, figures, points)?
        .first()
        .copied()
        .ok_or_else(|| {
            Error::unsupported_feature(
                "SQL Server sent an empty point, which `geo-types` cannot represent",
            )
        })
}

/// Returns the coordinates belonging to one figure.
///
/// A figure owns the points from its `Point Offset` up to the next figure's
/// offset (or the end of the point list).
fn figure_coords(
    figure_offset: i32,
    figures: &[Figure],
    points: &[Coord<f64>],
) -> Result<Vec<Coord<f64>>> {
    if figure_offset < 0 {
        return Ok(Vec::new());
    }

    let index = usize::try_from(figure_offset)
        .map_err(|_| Error::invalid_result("a spatial figure offset is negative"))?;
    let figure = figures.get(index).ok_or_else(|| {
        Error::invalid_result("a spatial figure offset is outside the figure table")
    })?;
    let start = figure.point_offset as usize;
    let end = figures
        .iter()
        .map(|other| other.point_offset as usize)
        .filter(|offset| *offset > start)
        .min()
        .unwrap_or(points.len());

    points
        .get(start..end)
        .map(<[Coord<f64>]>::to_vec)
        .ok_or_else(|| {
            Error::invalid_result("a spatial figure's points are outside the point table")
        })
}

/// Appends a shape and returns its index.
fn push_shape(shapes: &mut Vec<Shape>, parent: i32, figure_offset: i32, shape_type: u8) -> i32 {
    let index = shapes.len() as i32;
    shapes.push(Shape {
        parent,
        figure_offset,
        shape_type,
    });
    index
}

/// Appends a shape with no figures, which is how SQL Server encodes an empty
/// geometry.
fn push_empty_shape(shapes: &mut Vec<Shape>, parent: i32, shape_type: u8) -> i32 {
    push_shape(shapes, parent, -1, shape_type)
}

/// Splits a geometry into points, figures and shapes, returning its shape index
/// so a parent can reference it.
fn encode_geometry(
    geometry: &Geometry<f64>,
    parent: i32,
    points: &mut Vec<Coord<f64>>,
    figures: &mut Vec<Figure>,
    shapes: &mut Vec<Shape>,
) -> i32 {
    match geometry {
        Geometry::Point(point) => {
            let point_offset = points.len() as u32;
            points.push(point.0);
            let figure_offset = figures.len() as i32;
            figures.push(Figure {
                attribute: FIGURE_STROKE,
                point_offset,
            });
            push_shape(shapes, parent, figure_offset, SHAPE_POINT)
        }
        Geometry::Line(line) => {
            let point_offset = points.len() as u32;
            points.push(line.start);
            points.push(line.end);
            let figure_offset = figures.len() as i32;
            figures.push(Figure {
                attribute: FIGURE_STROKE,
                point_offset,
            });
            push_shape(shapes, parent, figure_offset, SHAPE_LINESTRING)
        }
        Geometry::LineString(line_string) => {
            encode_line_string(line_string, parent, points, figures, shapes)
        }
        Geometry::Polygon(polygon) => encode_polygon(polygon, parent, points, figures, shapes),
        Geometry::MultiPoint(multi_point) => {
            if multi_point.0.is_empty() {
                return push_empty_shape(shapes, parent, SHAPE_MULTIPOINT);
            }

            let shape_index = shapes.len() as i32;
            shapes.push(Shape {
                parent,
                figure_offset: figures.len() as i32,
                shape_type: SHAPE_MULTIPOINT,
            });

            for point in &multi_point.0 {
                let point_offset = points.len() as u32;
                points.push(point.0);
                let figure_offset = figures.len() as i32;
                figures.push(Figure {
                    attribute: FIGURE_STROKE,
                    point_offset,
                });
                push_shape(shapes, shape_index, figure_offset, SHAPE_POINT);
            }

            shape_index
        }
        Geometry::MultiLineString(multi_line_string) => {
            if multi_line_string.0.is_empty() {
                return push_empty_shape(shapes, parent, SHAPE_MULTILINESTRING);
            }

            let shape_index = shapes.len() as i32;
            shapes.push(Shape {
                parent,
                figure_offset: figures.len() as i32,
                shape_type: SHAPE_MULTILINESTRING,
            });

            for line_string in &multi_line_string.0 {
                encode_line_string(line_string, shape_index, points, figures, shapes);
            }

            shape_index
        }
        Geometry::MultiPolygon(multi_polygon) => {
            if multi_polygon.0.is_empty() {
                return push_empty_shape(shapes, parent, SHAPE_MULTIPOLYGON);
            }

            let shape_index = shapes.len() as i32;
            shapes.push(Shape {
                parent,
                figure_offset: figures.len() as i32,
                shape_type: SHAPE_MULTIPOLYGON,
            });

            for polygon in &multi_polygon.0 {
                encode_polygon(polygon, shape_index, points, figures, shapes);
            }

            shape_index
        }
        Geometry::GeometryCollection(collection) => {
            if collection.0.is_empty() {
                return push_empty_shape(shapes, parent, SHAPE_GEOMETRYCOLLECTION);
            }

            let shape_index = shapes.len() as i32;
            shapes.push(Shape {
                parent,
                figure_offset: figures.len() as i32,
                shape_type: SHAPE_GEOMETRYCOLLECTION,
            });

            for child in &collection.0 {
                encode_geometry(child, shape_index, points, figures, shapes);
            }

            shape_index
        }
        Geometry::Rect(rect) => encode_polygon(&rect.to_polygon(), parent, points, figures, shapes),
        Geometry::Triangle(triangle) => {
            encode_polygon(&triangle.to_polygon(), parent, points, figures, shapes)
        }
    }
}

/// Encodes a line string as a stroke figure, or as an empty shape.
fn encode_line_string(
    line_string: &LineString<f64>,
    parent: i32,
    points: &mut Vec<Coord<f64>>,
    figures: &mut Vec<Figure>,
    shapes: &mut Vec<Shape>,
) -> i32 {
    if line_string.0.is_empty() {
        return push_empty_shape(shapes, parent, SHAPE_LINESTRING);
    }

    let point_offset = points.len() as u32;
    points.extend_from_slice(&line_string.0);
    let figure_offset = figures.len() as i32;
    figures.push(Figure {
        attribute: FIGURE_STROKE,
        point_offset,
    });
    push_shape(shapes, parent, figure_offset, SHAPE_LINESTRING)
}

/// Encodes a polygon: its exterior ring is figure 2, its holes figure 0.
fn encode_polygon(
    polygon: &Polygon<f64>,
    parent: i32,
    points: &mut Vec<Coord<f64>>,
    figures: &mut Vec<Figure>,
    shapes: &mut Vec<Shape>,
) -> i32 {
    if polygon.exterior().0.is_empty() && polygon.interiors().iter().all(|ring| ring.0.is_empty()) {
        return push_empty_shape(shapes, parent, SHAPE_POLYGON);
    }

    let point_offset = points.len() as u32;
    points.extend_from_slice(&polygon.exterior().0);
    let figure_offset = figures.len() as i32;
    figures.push(Figure {
        attribute: FIGURE_EXTERIOR_RING,
        point_offset,
    });

    for interior in polygon.interiors() {
        if interior.0.is_empty() {
            continue;
        }
        let point_offset = points.len() as u32;
        points.extend_from_slice(&interior.0);
        figures.push(Figure {
            attribute: FIGURE_INTERIOR_RING,
            point_offset,
        });
    }

    push_shape(shapes, parent, figure_offset, SHAPE_POLYGON)
}

/// A bounds-checked little-endian reader over a spatial payload.
struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| Error::invalid_result("a spatial payload length overflowed"))?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| Error::invalid_result("the spatial payload is truncated"))?;
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Payloads captured from SQL Server 2022 over TDS.
    const POINT_4326: &str = "e6100000010c000000000000f03f0000000000000040";
    const LINESTRING_4326: &str = concat!(
        "e6100000010403000000000000000000f03f0000000000000040",
        "0000000000000840000000000000104000000000000014400000000000001840",
        "01000000010000000001000000ffffffff0000000002",
    );
    const POLYGON_WITH_HOLE_4326: &str = concat!(
        "e61000000104090000000000000000000000000000000000000000000000000010",
        "400000000000000000000000000000104000000000000010400000000000000000",
        "000000000000104000000000000000000000000000000000000000000000f03f0000",
        "00000000f03f0000000000000040000000000000f03f000000000000004000000000",
        "00000040000000000000f03f000000000000f03f0200000002000000000005000000",
        "01000000ffffffff0000000003",
    );
    const MULTIPOINT_4326: &str = concat!(
        "e6100000010402000000000000000000f03f0000000000000040",
        "0000000000000840000000000000104002000000010000000001",
        "0100000003000000ffffffff0000000004000000000000000001",
        "000000000100000001",
    );
    const MULTIPOLYGON_4326: &str = concat!(
        "e610000001040800000000000000000000000000000000000000000000000000f03f",
        "0000000000000000000000000000f03f000000000000f03f0000000000000000",
        "0000000000000000000000000000144000000000000014400000000000001840",
        "0000000000001440000000000000184000000000000018400000000000001440",
        "0000000000001440020000000200000000020400000003000000ffffffff00000000",
        "06000000000000000003000000000100000003",
    );
    const COLLECTION_4326: &str = concat!(
        "e6100000010403000000000000000000f03f0000000000000040",
        "0000000000000840000000000000104000000000000014400000000000001840",
        "020000000100000000010100000003000000ffffffff0000000007000000000000",
        "000001000000000100000002",
    );

    fn decode_hex(hex: &str) -> (i32, Geometry<f64>) {
        decode(&hex_bytes(hex)).unwrap()
    }

    #[test]
    fn decodes_single_point_and_keeps_srid() {
        let (srid, geometry) = decode_hex(POINT_4326);

        assert_eq!(srid, 4326);
        assert_eq!(geometry, Geometry::Point(Point::new(1.0, 2.0)));
    }

    #[test]
    fn decodes_linestring() {
        let (srid, geometry) = decode_hex(LINESTRING_4326);

        assert_eq!(srid, 4326);
        assert_eq!(
            geometry,
            Geometry::LineString(LineString::new(vec![
                Coord { x: 1.0, y: 2.0 },
                Coord { x: 3.0, y: 4.0 },
                Coord { x: 5.0, y: 6.0 },
            ]))
        );
    }

    #[test]
    fn decodes_polygon_with_a_hole() {
        let (_, geometry) = decode_hex(POLYGON_WITH_HOLE_4326);
        let Geometry::Polygon(polygon) = geometry else {
            panic!("expected a polygon");
        };

        assert_eq!(polygon.exterior().0.len(), 5);
        assert_eq!(polygon.interiors().len(), 1);
        assert_eq!(polygon.interiors()[0].0.len(), 4);
    }

    #[test]
    fn decodes_multi_part_shapes() {
        let (_, geometry) = decode_hex(MULTIPOINT_4326);
        assert_eq!(
            geometry,
            Geometry::MultiPoint(MultiPoint::new(vec![
                Point::new(1.0, 2.0),
                Point::new(3.0, 4.0),
            ]))
        );

        let (_, geometry) = decode_hex(MULTIPOLYGON_4326);
        let Geometry::MultiPolygon(multi_polygon) = geometry else {
            panic!("expected a multi-polygon");
        };
        assert_eq!(multi_polygon.0.len(), 2);

        let (_, geometry) = decode_hex(COLLECTION_4326);
        let Geometry::GeometryCollection(collection) = geometry else {
            panic!("expected a geometry collection");
        };
        assert_eq!(collection.0.len(), 2);
        assert!(matches!(collection.0[0], Geometry::Point(_)));
        assert!(matches!(collection.0[1], Geometry::LineString(_)));
    }

    #[test]
    fn round_trips_through_the_encoder() {
        let geometries = [
            Geometry::Point(Point::new(1.0, 2.0)),
            Geometry::LineString(LineString::new(vec![
                Coord { x: 1.0, y: 2.0 },
                Coord { x: 3.0, y: 4.0 },
            ])),
            Geometry::Polygon(Polygon::new(
                LineString::new(vec![
                    Coord { x: 0.0, y: 0.0 },
                    Coord { x: 4.0, y: 0.0 },
                    Coord { x: 4.0, y: 4.0 },
                    Coord { x: 0.0, y: 0.0 },
                ]),
                vec![LineString::new(vec![
                    Coord { x: 1.0, y: 1.0 },
                    Coord { x: 2.0, y: 1.0 },
                    Coord { x: 2.0, y: 2.0 },
                    Coord { x: 1.0, y: 1.0 },
                ])],
            )),
            Geometry::MultiPoint(MultiPoint::new(vec![
                Point::new(1.0, 2.0),
                Point::new(3.0, 4.0),
            ])),
            Geometry::MultiLineString(MultiLineString::new(vec![
                LineString::new(vec![Coord { x: 1.0, y: 2.0 }, Coord { x: 3.0, y: 4.0 }]),
                LineString::new(vec![Coord { x: 5.0, y: 6.0 }, Coord { x: 7.0, y: 8.0 }]),
            ])),
            Geometry::MultiPolygon(MultiPolygon::new(vec![
                Polygon::new(
                    LineString::new(vec![
                        Coord { x: 0.0, y: 0.0 },
                        Coord { x: 1.0, y: 0.0 },
                        Coord { x: 1.0, y: 1.0 },
                        Coord { x: 0.0, y: 0.0 },
                    ]),
                    Vec::new(),
                ),
                Polygon::new(
                    LineString::new(vec![
                        Coord { x: 5.0, y: 5.0 },
                        Coord { x: 6.0, y: 5.0 },
                        Coord { x: 6.0, y: 6.0 },
                        Coord { x: 5.0, y: 5.0 },
                    ]),
                    Vec::new(),
                ),
            ])),
            Geometry::GeometryCollection(GeometryCollection::new_from(vec![
                Geometry::Point(Point::new(1.0, 2.0)),
                Geometry::LineString(LineString::new(vec![
                    Coord { x: 3.0, y: 4.0 },
                    Coord { x: 5.0, y: 6.0 },
                ])),
            ])),
        ];

        for geometry in geometries {
            let bytes = encode(4326, &geometry);
            let (srid, decoded) = decode(&bytes).unwrap();

            assert_eq!(srid, 4326, "SRID did not round-trip for {geometry:?}");
            assert_eq!(decoded, geometry, "payload did not round-trip");
        }
    }

    #[test]
    fn rejects_measured_and_curved_values() {
        // POINTZ(1 2 3): the Z bit is set in the serialization properties.
        let point_z = "e6100000010d000000000000f03f00000000000000400000000000000840";
        let error = decode(&hex_bytes(point_z)).unwrap_err();
        assert!(error.is_unsupported_feature(), "got: {error}");

        // CIRCULARSTRING: version 2, which this codec does not model.
        let circular = concat!(
            "e610000002040300000000000000000000000000000000000000000000000000f03f",
            "000000000000f03f000000000000004000000000000000010000000200000000",
            "01000000ffffffff0000000008",
        );
        let error = decode(&hex_bytes(circular)).unwrap_err();
        assert!(error.is_unsupported_feature(), "got: {error}");
    }

    #[test]
    fn rejects_a_truncated_payload() {
        // A payload that stops mid-header, and one with nothing in it at all.
        for hex in ["e6100000", ""] {
            let error = decode(&hex_bytes(hex)).unwrap_err();

            assert!(
                error.to_string().contains("truncated"),
                "expected a truncated-payload error for `{hex}`, got: {error}"
            );
        }
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
            .collect()
    }
}
