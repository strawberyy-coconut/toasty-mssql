//! Converting Toasty values into TDS bind parameters, and rewriting SQL
//! parameter markers into the `@pN` names `sp_executesql` requires.

use mssql_tds::{
    datatypes::{decoder::DecimalParts, sql_string::SqlString, sqltypes::SqlType},
    message::parameters::rpc_parameters::{RpcParameter, StatusFlags},
};
use toasty_core::{Error, Result, driver::operation::TypedValue, schema::db, stmt};

/// Builds the RPC parameters for a statement's extracted bind parameters.
///
/// The renderer emits `@p1`, `@p2`, … for `Expr::Arg(n)` in that order, and
/// `sp_executesql` derives its declared parameter list from these names, so the
/// two must agree exactly.
pub(crate) fn rpc_params(params: &[TypedValue]) -> Result<Vec<RpcParameter>> {
    params
        .iter()
        .enumerate()
        .map(|(index, param)| {
            let sql_type = bind_value(&param.value, &param.ty)?;

            Ok(RpcParameter::new(
                Some(format!("@p{}", index + 1)),
                StatusFlags::NONE,
                sql_type,
            ))
        })
        .collect()
}

/// Converts a Toasty value into the TDS parameter value for its storage type.
pub(crate) fn bind_value(value: &stmt::Value, ty: &db::Type) -> Result<SqlType> {
    // A typed NULL keeps `sp_executesql`'s declared parameter list accurate, so
    // SQL Server can convert it to the target column.
    if matches!(value, stmt::Value::Null) {
        return Ok(null_value(ty));
    }

    // `#[column(type = ...)]` puts an opaque type name in the schema, which says
    // nothing about how the value travels. Bind such a value as its own type and
    // let SQL Server convert it to the column, which is what carries a
    // `varbinary(max)` payload into a `geometry` column and WKT text into one
    // too.
    //
    // `datetimeoffset` is the exception: it keeps the value's own offset, so a
    // zoned value travels as a temporal parameter rather than the `[IANA]`-tagged
    // text the escape hatch would otherwise send, which the server rejects.
    if let (stmt::Value::Zoned(value), db::Type::Custom(name)) = (value, ty)
        && super::temporal::is_datetimeoffset(name)
    {
        return match super::temporal::bind_datetimeoffset(value) {
            Some(value) => Ok(SqlType::DateTimeOffset(Some(value))),
            None => Err(Error::unsupported_feature(
                "SQL Server driver cannot bind this jiff::Zoned as DATETIMEOFFSET",
            )),
        };
    }

    if matches!(ty, db::Type::Custom(_))
        && let Some(natural) = natural_type(value)
    {
        return bind_value(value, &natural);
    }

    let sql_type = match (value, ty) {
        (stmt::Value::Bool(value), db::Type::Boolean) => SqlType::Bit(Some(*value)),

        // SQL Server's integer types are signed, so the unsigned widths are
        // declared as the signed type their column is rendered with.
        (stmt::Value::I8(value), db::Type::Integer(1)) => {
            SqlType::SmallInt(Some(i16::from(*value)))
        }
        (stmt::Value::I16(value), db::Type::Integer(2)) => SqlType::SmallInt(Some(*value)),
        (stmt::Value::I32(value), db::Type::Integer(4)) => SqlType::Int(Some(*value)),
        (stmt::Value::I64(value), db::Type::Integer(8)) => SqlType::BigInt(Some(*value)),

        (stmt::Value::U8(value), db::Type::UnsignedInteger(1)) => {
            SqlType::SmallInt(Some(i16::from(*value)))
        }
        (stmt::Value::U16(value), db::Type::UnsignedInteger(2)) => {
            SqlType::Int(Some(i32::from(*value)))
        }
        (stmt::Value::U32(value), db::Type::UnsignedInteger(4)) => {
            SqlType::BigInt(Some(i64::from(*value)))
        }
        (stmt::Value::U64(value), db::Type::UnsignedInteger(8)) => {
            match i64::try_from(*value) {
                Ok(value) => SqlType::BigInt(Some(value)),
                // The column is `DECIMAL(20, 0)`, which holds the full range,
                // but the driver does not encode decimals yet.
                Err(_) => {
                    return Err(Error::unsupported_feature(
                        "SQL Server driver cannot bind a u64 larger than i64::MAX yet; \
                         use a value that fits in an i64",
                    ));
                }
            }
        }

        (stmt::Value::F32(value), db::Type::Float(4)) => SqlType::Real(Some(*value)),
        (stmt::Value::F64(value), db::Type::Float(8)) => SqlType::Float(Some(*value)),
        // The two float widths interconvert: a plan may widen or narrow a value
        // to match the column's storage width.
        (stmt::Value::F32(value), db::Type::Float(8)) => SqlType::Float(Some(f64::from(*value))),
        (stmt::Value::F64(value), db::Type::Float(4)) => SqlType::Real(Some(*value as f32)),

        // An enum without a native column type stores the variant name.
        (stmt::Value::String(value), db::Type::Enum(_)) => {
            SqlType::NVarcharMax(Some(SqlString::from_utf8_string(value.clone())))
        }

        // `DECIMAL(p, s)` needs the precision and scale declared up front; the
        // TDS layer takes them from the value's textual form.
        //
        // A *parameter* does not always know them. A `#[document]` leaf's bound
        // operand arrives as `Numeric(None)`, because no column is behind it to
        // name a width, so the value's own digits have to serve.
        (stmt::Value::Decimal(value), db::Type::Numeric(declared)) => {
            let (precision, scale) = decimal_width(*declared, value.mantissa(), value.scale());
            let parts = DecimalParts::from_string(&value.to_string(), precision, scale).map_err(
                |error| {
                    Error::unsupported_feature(format!(
                        "SQL Server driver cannot encode {value} as DECIMAL({precision}, {scale}): {error}"
                    ))
                },
            )?;

            SqlType::Decimal(Some(parts))
        }
        (stmt::Value::BigDecimal(value), db::Type::Numeric(declared)) => {
            let (mantissa, scale) = value.as_bigint_and_exponent();
            let (precision, scale) = decimal_width(
                *declared,
                mantissa,
                u32::try_from(scale).unwrap_or(u32::MAX),
            );
            let parts = DecimalParts::from_string(&value.to_string(), precision, scale).map_err(
                |error| {
                    Error::unsupported_feature(format!(
                        "SQL Server driver cannot encode {value} as DECIMAL({precision}, {scale}): {error}"
                    ))
                },
            )?;

            SqlType::Decimal(Some(parts))
        }

        // A decimal stored as text, which is the driver's untyped default.
        (stmt::Value::Decimal(value), db::Type::Text) => {
            SqlType::NVarcharMax(Some(SqlString::from_utf8_string(value.to_string())))
        }
        (stmt::Value::BigDecimal(value), db::Type::Text) => {
            SqlType::NVarcharMax(Some(SqlString::from_utf8_string(value.to_string())))
        }

        (stmt::Value::String(value), db::Type::Text) => {
            SqlType::NVarcharMax(Some(SqlString::from_utf8_string(value.clone())))
        }
        (stmt::Value::String(value), db::Type::VarChar(len)) => {
            let len = u16::try_from(*len).map_err(|_| {
                Error::unsupported_feature(format!(
                    "SQL Server driver cannot bind a NVARCHAR({len}) parameter; \
                     the maximum declared length is {}",
                    u16::MAX
                ))
            })?;
            SqlType::NVarchar(Some(SqlString::from_utf8_string(value.clone())), len)
        }

        (stmt::Value::Bytes(value), db::Type::Blob) => SqlType::VarBinaryMax(Some(value.clone())),

        // A `Vec<scalar>` column is JSON text, so the whole list travels as one
        // string parameter; SQL Server's JSON functions read it back in place.
        // The element type is not named here — it is carried by the JSON itself
        // and by the engine's decode type.
        (stmt::Value::List(_), db::Type::List(_)) => {
            let json = toasty_sql::json::to_string(value).map_err(|error| {
                Error::unsupported_feature(format!(
                    "SQL Server driver cannot encode {value:?} as JSON for a collection column: {error}"
                ))
            })?;

            SqlType::NVarcharMax(Some(SqlString::from_utf8_string(json)))
        }

        // A `#[document]` column is JSON text, on the same terms. A document
        // field arrives as `Value::Object`; a `Vec<embed>` collection arrives as
        // a `Value::List` of them, because the engine collapses a list of
        // documents to a single document (`db::Type::list`), so both bind the
        // same way.
        (stmt::Value::Object(_) | stmt::Value::List(_), db::Type::Document { .. }) => {
            let json = toasty_sql::json::to_string(value).map_err(|error| {
                Error::unsupported_feature(format!(
                    "SQL Server driver cannot encode {value:?} as JSON for a document column: {error}"
                ))
            })?;

            SqlType::NVarcharMax(Some(SqlString::from_utf8_string(json)))
        }

        (stmt::Value::Uuid(value), db::Type::Uuid) => SqlType::Uuid(Some(*value)),
        // A UUID stored in text, which is what an explicit column-type override
        // or the `Zoned` default produces.
        (stmt::Value::Uuid(value), db::Type::Text) => {
            SqlType::NVarcharMax(Some(SqlString::from_utf8_string(value.to_string())))
        }

        // Temporal values have their own wire representation.
        (
            value @ (stmt::Value::Timestamp(_)
            | stmt::Value::DateTime(_)
            | stmt::Value::Date(_)
            | stmt::Value::Time(_)),
            _,
        ) => match (value, ty) {
            // Stored in a text column, a temporal value is written in the ISO
            // form its application type round-trips through.
            (value, db::Type::Text) => {
                SqlType::NVarcharMax(Some(SqlString::from_utf8_string(temporal_text(value)?)))
            }
            (value, _) => super::temporal::bind(value).ok_or_else(|| {
                Error::unsupported_feature(format!(
                    "SQL Server driver cannot bind {value:?} for a column of type {ty:?}"
                ))
            })?,
        },

        // A timezone-aware value keeps its zone, so it is stored as text.
        (stmt::Value::Zoned(value), db::Type::Text) => {
            SqlType::NVarcharMax(Some(SqlString::from_utf8_string(value.to_string())))
        }

        (value, ty) => {
            return Err(Error::unsupported_feature(format!(
                "SQL Server driver cannot bind {value:?} for a column of type {ty:?}"
            )));
        }
    };

    Ok(sql_type)
}

/// The storage type a value travels as when the column's own type says nothing.
///
/// Only used for [`db::Type::Custom`], whose name is opaque to the driver.
fn natural_type(value: &stmt::Value) -> Option<db::Type> {
    Some(match value {
        stmt::Value::Bool(_) => db::Type::Boolean,
        stmt::Value::I8(_) => db::Type::Integer(1),
        stmt::Value::I16(_) => db::Type::Integer(2),
        stmt::Value::I32(_) => db::Type::Integer(4),
        stmt::Value::I64(_) => db::Type::Integer(8),
        stmt::Value::U8(_) => db::Type::UnsignedInteger(1),
        stmt::Value::U16(_) => db::Type::UnsignedInteger(2),
        stmt::Value::U32(_) => db::Type::UnsignedInteger(4),
        stmt::Value::U64(_) => db::Type::UnsignedInteger(8),
        stmt::Value::F32(_) => db::Type::Float(4),
        stmt::Value::F64(_) => db::Type::Float(8),
        stmt::Value::String(_) => db::Type::Text,
        stmt::Value::Bytes(_) => db::Type::Blob,
        stmt::Value::Uuid(_) => db::Type::Uuid,
        // A decimal has no T-SQL text-to-`decimal` problem: the server parses
        // the text form, which is exact rather than a float approximation.
        stmt::Value::Decimal(_) | stmt::Value::BigDecimal(_) => db::Type::Text,
        stmt::Value::Timestamp(_) | stmt::Value::DateTime(_) => db::Type::DateTime(6),
        stmt::Value::Date(_) => db::Type::Date,
        stmt::Value::Time(_) => db::Type::Time(6),
        stmt::Value::Zoned(_) => db::Type::Text,
        _ => return None,
    })
}

/// The ISO text form of a temporal value, for storage in a text column.
fn temporal_text(value: &stmt::Value) -> Result<String> {
    Ok(match value {
        stmt::Value::Timestamp(value) => value.to_string(),
        stmt::Value::Zoned(value) => value.to_string(),
        stmt::Value::DateTime(value) => value.to_string(),
        stmt::Value::Date(value) => value.to_string(),
        stmt::Value::Time(value) => value.to_string(),
        other => {
            return Err(Error::unsupported_feature(format!(
                "SQL Server driver has no text form for {other:?}"
            )));
        }
    })
}

/// The TDS parameter value used to declare a typed `NULL`.
fn null_value(ty: &db::Type) -> SqlType {
    match ty {
        db::Type::Boolean => SqlType::Bit(None),
        db::Type::Integer(1) => SqlType::TinyInt(None),
        db::Type::Integer(2) => SqlType::SmallInt(None),
        db::Type::Integer(4) => SqlType::Int(None),
        db::Type::Integer(8) => SqlType::BigInt(None),
        db::Type::UnsignedInteger(1) => SqlType::SmallInt(None),
        db::Type::UnsignedInteger(2) => SqlType::Int(None),
        db::Type::UnsignedInteger(4) | db::Type::UnsignedInteger(8) => SqlType::BigInt(None),
        db::Type::Float(4) => SqlType::Real(None),
        db::Type::Float(8) => SqlType::Float(None),
        db::Type::Blob | db::Type::Binary(_) => SqlType::VarBinaryMax(None),
        db::Type::Uuid => SqlType::Uuid(None),
        // A temporal column declares its own type so the server converts the
        // `NULL` rather than guessing.
        other => match super::temporal::null_value(other) {
            Some(sql_type) => sql_type,
            // `nvarchar(max)` converts to most target types, so it is the safe
            // fallback for the types this driver does not bind yet.
            None => SqlType::NVarcharMax(None),
        },
    }
}

/// The precision and scale to declare a decimal parameter with.
///
/// A column names both, so the declared width is used as given. A parameter can
/// arrive without them — `Numeric(None)`, from a `#[document]` leaf — and then
/// the value's own digits are the only thing to go on: `19.99` is a
/// `DECIMAL(4, 2)`.
///
/// SQL Server caps the precision at 38, and the scale may not exceed it, so a
/// value like `0.001` (one significant digit, three decimal places) widens to
/// `DECIMAL(3, 3)` rather than the invalid `DECIMAL(1, 3)`.
fn decimal_width(declared: Option<(u32, u32)>, mantissa: impl ToString, scale: u32) -> (u8, u8) {
    let digits = mantissa.to_string().trim_start_matches('-').len();

    let (precision, scale) = match declared {
        Some(declared) => declared,
        None => (
            digits.max(scale as usize).min(u32::MAX as usize) as u32,
            scale,
        ),
    };

    let precision = u8::try_from(precision).unwrap_or(u8::MAX).min(38);
    let scale = u8::try_from(scale).unwrap_or(u8::MAX).min(precision);

    (precision, scale)
}

/// Rewrites ODBC-style positional `?` markers into the `@pN` names
/// `sp_executesql` requires.
///
/// User-authored SQL arrives with the placeholder syntax named by
/// [`Capability::sql_placeholder`](toasty_core::driver::Capability), which for
/// this driver is `?`. The scan is literal-aware: a `?` inside a string literal,
/// a quoted identifier or a comment is data, not a marker.
///
/// Adapted from the same-named routine in the Apache-2.0 licensed
/// `sqlx-mssql-rs` driver that ships in `old-sqlx/`.
pub(crate) fn rewrite_markers(sql: &str) -> (String, usize) {
    let mut output = String::with_capacity(sql.len());
    let mut markers = 0usize;
    let mut index = 0usize;

    while index < sql.len() {
        let rest = &sql[index..];
        let ch = rest.chars().next().unwrap_or('\u{fffd}');

        match ch {
            // `--` line comment: copied through to the end of the line.
            '-' if rest.starts_with("--") => {
                let end = rest.find('\n').map_or(sql.len(), |offset| index + offset);
                output.push_str(&sql[index..end]);
                index = end;
            }

            // `/* ... */` block comment.
            '/' if rest.starts_with("/*") => {
                let end = rest
                    .find("*/")
                    .map_or(sql.len(), |offset| index + offset + 2);
                output.push_str(&sql[index..end]);
                index = end;
            }

            '\'' => copy_quoted(sql, &mut output, &mut index, '\''),
            '"' => copy_quoted(sql, &mut output, &mut index, '"'),
            '[' => copy_quoted(sql, &mut output, &mut index, ']'),

            // A positional marker. A `?N` numbering is accepted too, since
            // `NumberedQuestionMark` is what the capability advertises.
            '?' => {
                markers += 1;
                output.push_str("@p");
                output.push_str(&markers.to_string());

                index += 1;
                while sql[index..].starts_with(|c: char| c.is_ascii_digit()) {
                    index += 1;
                }
            }

            other => {
                output.push(other);
                index += other.len_utf8();
            }
        }
    }

    (output, markers)
}

/// Copies a quoted region verbatim, including its delimiters.
///
/// `closing` ends the region; doubling it escapes it.
fn copy_quoted(sql: &str, output: &mut String, index: &mut usize, closing: char) {
    let opening = sql[*index..].chars().next().unwrap_or('\u{fffd}');
    output.push(opening);
    *index += opening.len_utf8();

    while *index < sql.len() {
        let ch = sql[*index..].chars().next().unwrap_or('\u{fffd}');

        if ch == closing {
            let after = *index + ch.len_utf8();

            // A doubled delimiter is an escaped character, not the end.
            if sql[after..].starts_with(closing) {
                output.push(closing);
                output.push(closing);
                *index = after + closing.len_utf8();
                continue;
            }

            output.push(closing);
            *index = after;
            break;
        }

        output.push(ch);
        *index += ch.len_utf8();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_positional_markers_in_source_order() {
        let (sql, count) = rewrite_markers("SELECT ? , ? FROM [t]");

        assert_eq!(sql, "SELECT @p1 , @p2 FROM [t]");
        assert_eq!(count, 2);
    }

    #[test]
    fn rewrites_numbered_markers() {
        let (sql, count) = rewrite_markers("SELECT ?1, ?2");

        assert_eq!(sql, "SELECT @p1, @p2");
        assert_eq!(count, 2);
    }

    #[test]
    fn leaves_markers_inside_literals_and_identifiers_alone() {
        let (sql, count) = rewrite_markers("SELECT '?', [?], ? FROM [a?b]");

        assert_eq!(sql, "SELECT '?', [?], @p1 FROM [a?b]");
        assert_eq!(count, 1);
    }

    #[test]
    fn leaves_markers_inside_comments_alone() {
        let (sql, count) = rewrite_markers("SELECT ? -- ?\n, ? /* ? */");

        assert_eq!(sql, "SELECT @p1 -- ?\n, @p2 /* ? */");
        assert_eq!(count, 2);
    }

    #[test]
    fn binds_scalar_values() {
        assert_eq!(
            bind_value(&stmt::Value::Bool(true), &db::Type::Boolean).unwrap(),
            SqlType::Bit(Some(true))
        );
        assert_eq!(
            bind_value(&stmt::Value::I64(7), &db::Type::Integer(8)).unwrap(),
            SqlType::BigInt(Some(7))
        );
        assert_eq!(
            bind_value(&stmt::Value::Null, &db::Type::Integer(8)).unwrap(),
            SqlType::BigInt(None)
        );
    }

    #[test]
    fn rejects_unbindable_combinations() {
        let error = bind_value(&stmt::Value::String("x".into()), &db::Type::Integer(8))
            .expect_err("must reject");

        assert!(error.is_unsupported_feature(), "got: {error}");
    }
}
