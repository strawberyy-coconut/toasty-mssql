//! Converting TDS result values into Toasty values.

use mssql_tds::datatypes::column_values::ColumnValues;

use toasty_core::{Error, Result, stmt};

/// Decodes a result value as the type the plan expects.
///
/// The plan names the type of every projected column, so this is the precise
/// path: a `SMALLINT` column backing a `u8` field decodes to
/// [`stmt::Value::U8`], not to the value's wire width.
pub(crate) fn decode(value: &ColumnValues, expected: &stmt::Type) -> Result<stmt::Value> {
    coerce(infer(value)?, expected)
}

/// Decodes a result value from its wire type alone.
///
/// Used when the plan cannot name the type, such as `RawSqlRet::Infer`, so the
/// result is the value as the server stored it: a bit stays the `0`/`1` T-SQL
/// has instead of a boolean, and a `uniqueidentifier` is its 16 bytes.
pub(crate) fn infer(value: &ColumnValues) -> Result<stmt::Value> {
    let value = match value {
        ColumnValues::Null => stmt::Value::Null,

        ColumnValues::Bit(value) => stmt::Value::I64(i64::from(*value)),

        // SQL Server's `TINYINT` is unsigned, so it widens to `i16`.
        ColumnValues::TinyInt(value) => stmt::Value::I16(i16::from(*value)),
        ColumnValues::SmallInt(value) => stmt::Value::I16(*value),
        ColumnValues::Int(value) => stmt::Value::I32(*value),
        ColumnValues::BigInt(value) => stmt::Value::I64(*value),

        ColumnValues::Real(value) => stmt::Value::F32(*value),
        ColumnValues::Float(value) => stmt::Value::F64(*value),

        ColumnValues::String(value) => stmt::Value::String(value.to_utf8_string()),

        ColumnValues::Bytes(value) => stmt::Value::Bytes(value.clone()),

        ColumnValues::Uuid(value) => stmt::Value::Bytes(value.as_bytes().to_vec()),

        // The TDS layer exposes a decimal's textual form, which is the exact
        // value rather than a float approximation.
        ColumnValues::Decimal(parts) | ColumnValues::Numeric(parts) => {
            stmt::Value::Decimal(parse_decimal(&parts.to_decimal_string())?)
        }

        other => super::temporal::decode(other).ok_or_else(|| {
            Error::unsupported_feature(format!(
                "SQL Server driver does not decode the value {other:?} yet"
            ))
        })?,
    };

    Ok(value)
}

/// Narrows an inferred value to the type the plan expects.
fn coerce(value: stmt::Value, expected: &stmt::Type) -> Result<stmt::Value> {
    use stmt::Type;

    if matches!(value, stmt::Value::Null) {
        return Ok(value);
    }

    // A predicate projected as a value arrives as a bit, which TDS reports as an
    // integer because `CASE … THEN 1 ELSE 0` has no boolean type in T-SQL.
    if matches!(expected, stmt::Type::Bool)
        && let Some(integer) = integral_value(&value)
    {
        return Ok(stmt::Value::Bool(integer != 0));
    }

    // Any integer the wire produced can satisfy any integer column as long as it
    // fits: a `u32` field rides a `BIGINT` column and comes back narrowed, and a
    // `u64` field rides `DECIMAL(20, 0)` and arrives as an integral decimal.
    if is_integer(expected)
        && let Some(integer) = integral_value(&value)
    {
        return match expected {
            Type::I8 => Ok(stmt::Value::I8(narrow(integer, "i8")?)),
            Type::I16 => Ok(stmt::Value::I16(narrow(integer, "i16")?)),
            Type::I32 => Ok(stmt::Value::I32(narrow(integer, "i32")?)),
            Type::I64 => Ok(stmt::Value::I64(narrow(integer, "i64")?)),
            Type::U8 => Ok(stmt::Value::U8(narrow(integer, "u8")?)),
            Type::U16 => Ok(stmt::Value::U16(narrow(integer, "u16")?)),
            Type::U32 => Ok(stmt::Value::U32(narrow(integer, "u32")?)),
            Type::U64 => Ok(stmt::Value::U64(narrow(integer, "u64")?)),
            other => Err(unsupported_conversion(&value, other)),
        };
    }

    let coerced = match (value, expected) {
        (value @ stmt::Value::Bool(_), Type::Bool) => value,

        (value @ stmt::Value::F32(_), Type::F32) => value,
        (stmt::Value::F32(value), Type::F64) => stmt::Value::F64(f64::from(value)),
        (value @ stmt::Value::F64(_), Type::F64) => value,
        // A `f32` field rides a `REAL` column, but a wider wire value has to
        // come back narrowed when the column is a `FLOAT`.
        (stmt::Value::F64(value), Type::F32) => stmt::Value::F32(value as f32),

        // A decimal column can be projected as a decimal, or as text when the
        // application type is stored without a native numeric column.
        (
            value @ (stmt::Value::Decimal(_) | stmt::Value::BigDecimal(_)),
            Type::Decimal | Type::BigDecimal,
        ) => value,
        (stmt::Value::Decimal(value), Type::String) => stmt::Value::String(value.to_string()),
        (stmt::Value::BigDecimal(value), Type::String) => stmt::Value::String(value.to_string()),

        // `DATETIME2` is this driver's storage for both an instant and a civil
        // datetime, and both are written as UTC.
        (value @ stmt::Value::Timestamp(_), Type::Timestamp) => value,
        (value @ stmt::Value::DateTime(_), Type::DateTime) => value,
        (stmt::Value::Timestamp(value), Type::DateTime) => {
            stmt::Value::DateTime(super::temporal::utc_datetime(value))
        }
        (stmt::Value::DateTime(value), Type::Timestamp) => stmt::Value::Timestamp(
            super::temporal::timestamp_from_utc_datetime(value).ok_or_else(|| {
                Error::unsupported_feature(format!("{value} is not a representable instant"))
            })?,
        ),
        // A `datetimeoffset` column decodes to a zoned value, so a field that
        // wants only the instant — a `jiff::Timestamp` carrying an explicit
        // `#[column(type = "DATETIMEOFFSET")]` — is narrowed here. The offset is
        // dropped, because the field has nowhere to put it.
        (stmt::Value::Zoned(value), Type::Timestamp) => stmt::Value::Timestamp(value.timestamp()),

        // A temporal value stored in a text column round-trips through its ISO
        // form.
        (stmt::Value::Timestamp(value), Type::String) => stmt::Value::String(value.to_string()),
        (stmt::Value::DateTime(value), Type::String) => stmt::Value::String(value.to_string()),
        (stmt::Value::Zoned(value), Type::String) => stmt::Value::String(value.to_string()),
        (stmt::Value::Date(value), Type::String) => stmt::Value::String(value.to_string()),
        (stmt::Value::Time(value), Type::String) => stmt::Value::String(value.to_string()),
        (stmt::Value::String(value), Type::Timestamp) => {
            stmt::Value::Timestamp(value.parse().map_err(|error| {
                Error::unsupported_feature(format!(
                    "SQL Server returned `{value}`, which is not a timestamp: {error}"
                ))
            })?)
        }
        (stmt::Value::String(value), Type::Zoned) => {
            stmt::Value::Zoned(value.parse().map_err(|error| {
                Error::unsupported_feature(format!(
                    "SQL Server returned `{value}`, which is not a zoned datetime: {error}"
                ))
            })?)
        }
        (value @ stmt::Value::Date(_), Type::Date) => value,
        (value @ stmt::Value::Time(_), Type::Time) => value,
        (value @ stmt::Value::Zoned(_), Type::Zoned) => value,

        (value @ stmt::Value::String(_), Type::String) => value,
        (stmt::Value::Uuid(value), Type::String) => stmt::Value::String(value.to_string()),

        (value @ stmt::Value::Bytes(_), Type::Bytes) => value,

        (value @ stmt::Value::Uuid(_), Type::Uuid) => value,
        // Storage level, a `uniqueidentifier` is its 16 bytes.
        (stmt::Value::Bytes(value), Type::Uuid) => {
            stmt::Value::Uuid(uuid::Uuid::from_slice(&value).map_err(|error| {
                Error::unsupported_feature(format!(
                    "SQL Server returned {value:?}, which is not a UUID: {error}"
                ))
            })?)
        }
        (stmt::Value::String(value), Type::Uuid) => {
            stmt::Value::Uuid(value.parse().map_err(|error| {
                Error::unsupported_feature(format!(
                    "SQL Server returned `{value}`, which is not a UUID: {error}"
                ))
            })?)
        }

        // A `Vec<scalar>` column is JSON text, so the text is parsed back with
        // the element type the plan named. Going through the plan's type rather
        // than the wire type is what distinguishes a `Vec<String>` from a
        // `Vec<Uuid>`: both are JSON arrays of strings.
        (stmt::Value::String(value), Type::List(elem)) => {
            toasty_sql::json::list_from_str(&value, elem).map_err(|error| {
                Error::unsupported_feature(format!(
                    "SQL Server returned `{value}`, which is not a JSON array of {elem:?}: {error}"
                ))
            })?
        }

        // A `#[document]` column is JSON text too, typed structurally: the
        // engine raises the decoded object to the embed's positional record, so
        // the shape is decoded as stored and the leaf types are the engine's
        // business. A `Vec<embed>` collection arrives as `Type::List(Object)`,
        // which the list arm above already handles.
        (stmt::Value::String(value), Type::Object) => toasty_sql::json::from_str(&value, expected)
            .map_err(|error| {
                Error::unsupported_feature(format!(
                    "SQL Server returned `{value}`, which is not a JSON document: {error}"
                ))
            })?,

        (value, expected) => return Err(unsupported_conversion(&value, expected)),
    };

    Ok(coerced)
}
/// Parses the textual form of a decimal the server returned.
fn parse_decimal(text: &str) -> Result<rust_decimal::Decimal> {
    text.parse().map_err(|error| {
        Error::unsupported_feature(format!(
            "SQL Server returned `{text}`, which is not a decimal this driver can represent: {error}"
        ))
    })
}

/// Whether a type is one of the integer widths.
fn is_integer(ty: &stmt::Type) -> bool {
    matches!(
        ty,
        stmt::Type::I8
            | stmt::Type::I16
            | stmt::Type::I32
            | stmt::Type::I64
            | stmt::Type::U8
            | stmt::Type::U16
            | stmt::Type::U32
            | stmt::Type::U64
    )
}

/// Extracts an integer from a value that stands for one, including the decimal a
/// wide unsigned column is stored in.
fn integral_value(value: &stmt::Value) -> Option<i128> {
    use rust_decimal::prelude::ToPrimitive as _;

    if let Some(integer) = as_i128(value) {
        return Some(integer);
    }

    match value {
        stmt::Value::Decimal(value) => value.to_i128(),
        _ => None,
    }
}

/// Extracts an integer from any integer-valued [`stmt::Value`].
fn as_i128(value: &stmt::Value) -> Option<i128> {
    Some(match value {
        stmt::Value::I8(value) => i128::from(*value),
        stmt::Value::I16(value) => i128::from(*value),
        stmt::Value::I32(value) => i128::from(*value),
        stmt::Value::I64(value) => i128::from(*value),
        stmt::Value::U8(value) => i128::from(*value),
        stmt::Value::U16(value) => i128::from(*value),
        stmt::Value::U32(value) => i128::from(*value),
        stmt::Value::U64(value) => i128::from(*value),
        _ => return None,
    })
}

/// Narrows an integer to a target width, reporting overflow.
fn narrow<T: TryFrom<i128>>(value: i128, target: &str) -> Result<T> {
    T::try_from(value).map_err(|_| {
        Error::unsupported_feature(format!(
            "SQL Server returned a value out of range for a {target} column"
        ))
    })
}

fn unsupported_conversion(value: &stmt::Value, expected: &stmt::Type) -> Error {
    Error::unsupported_feature(format!(
        "SQL Server driver cannot decode {value:?} as {expected:?}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_typed_scalars() {
        assert_eq!(
            decode(&ColumnValues::Bit(true), &stmt::Type::Bool).unwrap(),
            stmt::Value::Bool(true)
        );
        assert_eq!(
            decode(&ColumnValues::BigInt(41), &stmt::Type::I64).unwrap(),
            stmt::Value::I64(41)
        );
        // A `u32` column is stored as `BIGINT`, so the wire value is wider than
        // the application type.
        assert_eq!(
            decode(&ColumnValues::BigInt(41), &stmt::Type::U32).unwrap(),
            stmt::Value::U32(41)
        );
        assert_eq!(
            decode(&ColumnValues::Float(1.5), &stmt::Type::F64).unwrap(),
            stmt::Value::F64(1.5)
        );
        assert_eq!(
            decode(&ColumnValues::Null, &stmt::Type::I64).unwrap(),
            stmt::Value::Null
        );
    }

    #[test]
    fn rejects_out_of_range_values() {
        let error = decode(&ColumnValues::BigInt(-1), &stmt::Type::U32).expect_err("must reject");
        assert!(error.is_unsupported_feature(), "got: {error}");
    }

    #[test]
    fn infers_from_the_wire_type() {
        assert_eq!(infer(&ColumnValues::Int(7)).unwrap(), stmt::Value::I32(7));
    }
}
