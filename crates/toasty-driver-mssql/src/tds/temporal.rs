//! Conversions between `jiff` civil types and the temporal values `mssql-tds`
//! binds and returns.
//!
//! T-SQL counts dates from `0001-01-01` and times in 100-nanosecond ticks,
//! neither of which `jiff` exposes directly, so both conversions are done here.
//!
//! The day-number arithmetic is the standard days-from-civil calculation for the
//! proleptic Gregorian calendar, adapted from the Apache-2.0 licensed
//! `sqlx-mssql-rs` driver that ships in `old-sqlx/`.

use jiff::{
    Timestamp, Zoned,
    civil::{Date, DateTime, Time},
    tz::{Offset, TimeZone},
};
use mssql_tds::datatypes::column_values::{
    ColumnValues, SqlDate, SqlDateTime2, SqlDateTimeOffset, SqlTime,
};

use toasty_core::{Result, stmt};

/// Days from `0001-01-01` to the Unix epoch, which T-SQL treats as day zero.
const DAYS_FROM_YEAR_ONE_TO_EPOCH: i64 = 719_162;

const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// A microsecond, and the number of 100-nanosecond ticks in one. The driver's
/// temporal columns are declared `(6)`, so a value is truncated to a whole
/// microsecond before it is bound.
const NANOS_PER_MICROSECOND: u64 = 1_000;
const TICKS_PER_MICROSECOND: u64 = 10;

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;

/// Days from `1970-01-01` for a civil date.
fn unix_days_from_date(date: Date) -> i64 {
    let year = i64::from(date.year());
    let month = i64::from(date.month());
    let day = i64::from(date.day());

    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let month_shift = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_shift + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`unix_days_from_date`].
fn date_from_unix_days(unix_days: i64) -> Option<Date> {
    let shifted = unix_days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_shift = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_shift + 2) / 5 + 1;
    let month = if month_shift < 10 {
        month_shift + 3
    } else {
        month_shift - 9
    };
    let year = year + i64::from(month <= 2);

    Date::new(
        i16::try_from(year).ok()?,
        i8::try_from(month).ok()?,
        i8::try_from(day).ok()?,
    )
    .ok()
}

/// The T-SQL day number for a civil date.
pub(crate) fn days_from_date(date: Date) -> Result<u32> {
    u32::try_from(unix_days_from_date(date) + DAYS_FROM_YEAR_ONE_TO_EPOCH).map_err(|_| {
        toasty_core::Error::unsupported_feature(format!(
            "{date} is outside the range SQL Server's DATE accepts"
        ))
    })
}

/// The civil date for a T-SQL day number.
pub(crate) fn date_from_days(days: u32) -> Option<Date> {
    date_from_unix_days(i64::from(days) - DAYS_FROM_YEAR_ONE_TO_EPOCH)
}

/// Nanoseconds since midnight for a civil time.
fn nanos_from_time(time: Time) -> Option<u64> {
    let seconds = u64::try_from(time.hour()).ok()? * SECONDS_PER_HOUR
        + u64::try_from(time.minute()).ok()? * SECONDS_PER_MINUTE
        + u64::try_from(time.second()).ok()?;

    Some(seconds * NANOS_PER_SECOND + u64::try_from(time.subsec_nanosecond()).ok()?)
}

/// The civil time for nanoseconds since midnight.
fn time_from_nanos(nanoseconds: u64) -> Option<Time> {
    let total_seconds = nanoseconds / NANOS_PER_SECOND;
    let subsecond = i32::try_from(nanoseconds % NANOS_PER_SECOND).ok()?;

    Time::new(
        i8::try_from(total_seconds / SECONDS_PER_HOUR).ok()?,
        i8::try_from((total_seconds % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE).ok()?,
        i8::try_from(total_seconds % SECONDS_PER_MINUTE).ok()?,
        subsecond,
    )
    .ok()
}

/// Binds a civil time. `TIME` carries 100-nanosecond ticks, but this driver's
/// temporal columns are declared `(6)` — microseconds — so anything finer is
/// truncated rather than sent for the server to round.
///
/// Truncating is what keeps the two ways a temporal can be stored in agreement.
/// A `#[document]` column holds the ISO text that `toasty_sql`'s JSON encoder
/// writes, and that encoder truncates to microseconds. A parameter carrying a
/// 100-nanosecond digit would round to a *different* microsecond and compare
/// unequal to the text it was meant to match.
pub(crate) fn time_from_jiff(time: Time) -> Option<SqlTime> {
    let nanoseconds = nanos_from_time(time)?;

    Some(SqlTime {
        time_nanoseconds: (nanoseconds / NANOS_PER_MICROSECOND) * TICKS_PER_MICROSECOND,
        scale: 7,
    })
}

/// Decodes a T-SQL time into a civil time.
pub(crate) fn time_to_jiff(value: &SqlTime) -> Option<Time> {
    time_from_nanos(value.time_nanoseconds.saturating_mul(100))
}

/// Binds a civil date and time.
pub(crate) fn datetime2_from_jiff(value: DateTime) -> Option<SqlDateTime2> {
    Some(SqlDateTime2 {
        days: days_from_date(value.date()).ok()?,
        time: time_from_jiff(value.time())?,
    })
}

/// Decodes a `DATETIME2` into a civil date and time.
pub(crate) fn datetime_from_datetime2(value: &SqlDateTime2) -> Option<DateTime> {
    let date = date_from_days(value.days)?;
    let time = time_to_jiff(&value.time)?;

    DateTime::from_parts(date, time).into()
}

/// The UTC civil date and time for an instant, which is how a timestamp is
/// stored and how it is read back.
pub(crate) fn utc_datetime(instant: Timestamp) -> DateTime {
    instant.to_zoned(TimeZone::UTC).datetime()
}

/// Reads a naively-stored timestamp as an instant in UTC.
pub(crate) fn timestamp_from_utc_datetime(value: DateTime) -> Option<Timestamp> {
    value
        .to_zoned(TimeZone::UTC)
        .ok()
        .map(|zoned| zoned.timestamp())
}

/// The column type name this driver recognises as SQL Server's
/// `datetimeoffset`.
///
/// `db::Type` has no offset variant, so a `Zoned` field reaches this column
/// type through `db::Type::Custom`, either from the driver's own
/// [`MssqlDateTimeOffset`](https://docs.rs/toasty-driver-mssql-ext/latest/toasty_driver_mssql_ext/struct.MssqlDateTimeOffset.html)
/// or from an explicit `#[column(type = "DATETIMEOFFSET")]`.
pub const DATETIMEOFFSET: &str = "datetimeoffset";

/// Whether a `db::Type::Custom` name is this driver's `datetimeoffset`.
pub(crate) fn is_datetimeoffset(name: &str) -> bool {
    name.eq_ignore_ascii_case(DATETIMEOFFSET)
}

/// Binds a zoned value as a `datetimeoffset` parameter.
///
/// The value's own offset is kept, because `mssql-tds` pairs a UTC-normalised
/// `datetime2` with the offset that applied at that instant — the same pairing
/// it hands back when decoding.
pub(crate) fn bind_datetimeoffset(value: &Zoned) -> Option<SqlDateTimeOffset> {
    Some(SqlDateTimeOffset {
        datetime2: datetime2_from_jiff(utc_datetime(value.timestamp()))?,
        offset: i16::try_from(value.offset().seconds() / 60).ok()?,
    })
}

/// Reads a `DATETIMEOFFSET` back as a zoned value.
///
/// `mssql-tds` normalises the time part to UTC and keeps the original offset
/// beside it, so the offset must not be applied again to recover the instant;
/// it is reattached as a fixed-offset zone, which is all the column carries.
fn zoned_from_datetimeoffset(value: &SqlDateTimeOffset) -> Option<Zoned> {
    let instant = timestamp_from_utc_datetime(datetime_from_datetime2(&value.datetime2)?)?;
    let offset = Offset::from_seconds(i32::from(value.offset) * 60).ok()?;

    Some(instant.to_zoned(TimeZone::fixed(offset)))
}

/// Converts an application-level temporal value into the TDS value to bind.
pub(crate) fn bind(value: &stmt::Value) -> Option<mssql_tds::datatypes::sqltypes::SqlType> {
    use mssql_tds::datatypes::sqltypes::SqlType;

    let sql_type = match value {
        stmt::Value::Timestamp(instant) => {
            SqlType::DateTime2(Some(datetime2_from_jiff(utc_datetime(*instant))?))
        }
        stmt::Value::DateTime(value) => SqlType::DateTime2(Some(datetime2_from_jiff(*value)?)),
        stmt::Value::Date(value) => {
            SqlType::Date(Some(SqlDate::create(days_from_date(*value).ok()?).ok()?))
        }
        stmt::Value::Time(value) => SqlType::Time(Some(time_from_jiff(*value)?)),
        // A zoned value has no temporal column type of its own: it binds as
        // `datetimeoffset` only where the column says so (`bind_datetimeoffset`),
        // and travels as text everywhere else.
        stmt::Value::Zoned(_) => return None,
        _ => return None,
    };

    Some(sql_type)
}

/// Converts a result value into an application-level temporal value.
pub(crate) fn decode(value: &ColumnValues) -> Option<stmt::Value> {
    let value = match value {
        ColumnValues::Date(value) => stmt::Value::Date(date_from_days(value.get_days())?),
        ColumnValues::Time(value) => stmt::Value::Time(time_to_jiff(value)?),
        // `DATETIME2` is this driver's storage for both `Timestamp` and
        // `DateTime`, and both are written as UTC.
        ColumnValues::DateTime2(value) => stmt::Value::DateTime(datetime_from_datetime2(value)?),
        // `mssql-tds` normalises the time part of a `DATETIMEOFFSET` to UTC and
        // keeps the original offset beside it, so the offset must not be applied
        // again to recover the instant. It decodes to a zoned value, which is
        // what the column's offset makes it; a field that wants only the instant
        // — or only text — is narrowed by the storage bridge in
        // [`super::decode`].
        ColumnValues::DateTimeOffset(value) => {
            stmt::Value::Zoned(zoned_from_datetimeoffset(value)?)
        }
        _ => return None,
    };

    Some(value)
}

/// The TDS type text for a temporal column, used when binding a `NULL`.
pub(crate) fn null_value(
    ty: &toasty_core::schema::db::Type,
) -> Option<mssql_tds::datatypes::sqltypes::SqlType> {
    use mssql_tds::datatypes::sqltypes::SqlType;
    use toasty_core::schema::db;

    let sql_type = match ty {
        db::Type::Date => SqlType::Date(None),
        db::Type::Time(_) => SqlType::Time(None),
        db::Type::Timestamp(_) | db::Type::DateTime(_) => SqlType::DateTime2(None),
        db::Type::Custom(name) if is_datetimeoffset(name) => SqlType::DateTimeOffset(None),
        _ => return None,
    };

    Some(sql_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_numbers_match_sql_server() {
        // `0001-01-01` is day zero, and the Unix epoch is 719_162 days later.
        assert_eq!(days_from_date(Date::constant(1, 1, 1)).unwrap(), 0);
        assert_eq!(days_from_date(Date::constant(1970, 1, 1)).unwrap(), 719_162);
        assert_eq!(unix_days_from_date(Date::constant(1970, 1, 1)), 0);
    }

    #[test]
    fn day_numbers_round_trip() {
        for days in [0, 1, 719_162, 1_000_000, 2_932_896] {
            let date = date_from_days(days).expect("day number must be representable");
            assert_eq!(days_from_date(date).unwrap(), days, "day {days}");
        }
    }

    #[test]
    fn times_round_trip() {
        let time = Time::constant(13, 45, 30, 123_456_000);
        let sql_time = time_from_jiff(time).expect("time must convert");

        assert_eq!(time_to_jiff(&sql_time).unwrap(), time);
    }

    #[test]
    fn datetimes_round_trip() {
        let datetime = DateTime::constant(2026, 9, 26, 13, 45, 30, 500_000_000);
        let sql_value = datetime2_from_jiff(datetime).expect("datetime must convert");

        assert_eq!(datetime_from_datetime2(&sql_value).unwrap(), datetime);
    }

    #[test]
    fn recognises_the_datetimeoffset_column_name() {
        assert!(is_datetimeoffset("DATETIMEOFFSET"));
        assert!(is_datetimeoffset("datetimeoffset"));
        assert!(!is_datetimeoffset("DATETIME2"));
    }

    #[test]
    fn datetimeoffsets_keep_the_instant_and_the_offset_but_not_the_zone() {
        let zoned: Zoned = "2021-06-15T14:30:00-04:00[America/New_York]"
            .parse()
            .expect("a valid zoned value");

        let sql_value = bind_datetimeoffset(&zoned).expect("must convert to datetimeoffset");
        assert_eq!(sql_value.offset, -4 * 60);

        let back = zoned_from_datetimeoffset(&sql_value).expect("must convert back");

        // The instant and the offset survive; the IANA name does not, because
        // the column type has no field for it.
        assert_eq!(back.timestamp(), zoned.timestamp());
        assert_eq!(back.offset(), zoned.offset());
        assert_ne!(back.to_string(), zoned.to_string());
    }
}
