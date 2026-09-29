//! Date and time functions — Microsoft's [Date and Time Functions][ref]
//! category.
//!
//! These apply to the `jiff` types Toasty stores as `DATETIME2` and `DATE`. They
//! are not offered on `TIME` columns, where most of them have no meaning: T-SQL's
//! `YEAR`, `EOMONTH` and friends do not accept a `time` value.
//!
//! T-SQL is inconsistent about where the subject goes: `YEAR(col)` and
//! `EOMONTH(col)` take it first, while `DATEPART(day, col)` and
//! `DATEADD(day, 1, col)` take it last.
//!
//! [ref]: https://learn.microsoft.com/en-us/sql/t-sql/functions/date-and-time-data-types-and-functions-transact-sql

use toasty::stmt::{Expr, Path};
use toasty_core::stmt;

use super::{Operand, call, call_last};

/// A date part keyword, for the functions that name one.
///
/// These are the units `DATEADD`, `DATEPART` and `DATETRUNC` accept. They
/// travel as bare keywords rather than quoted strings, because that is what the
/// T-SQL grammar takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatePart {
    /// `year`.
    Year,
    /// `quarter`.
    Quarter,
    /// `month`.
    Month,
    /// `dayofyear`.
    DayOfYear,
    /// `day`.
    Day,
    /// `week`.
    Week,
    /// `weekday`. Its result depends on the session's `DATEFIRST`, so the same
    /// date can yield a different number on a different connection.
    Weekday,
    /// `hour`.
    Hour,
    /// `minute`.
    Minute,
    /// `second`.
    Second,
    /// `millisecond`.
    Millisecond,
    /// `microsecond`.
    Microsecond,
    /// `nanosecond`. A `DATETIME2(6)` column has no nanosecond digits to give,
    /// so the last three are always `0`.
    Nanosecond,
}

impl DatePart {
    /// The keyword T-SQL spells this part with.
    fn keyword(self) -> &'static str {
        match self {
            Self::Year => "year",
            Self::Quarter => "quarter",
            Self::Month => "month",
            Self::DayOfYear => "dayofyear",
            Self::Day => "day",
            Self::Week => "week",
            Self::Weekday => "weekday",
            Self::Hour => "hour",
            Self::Minute => "minute",
            Self::Second => "second",
            Self::Millisecond => "millisecond",
            Self::Microsecond => "microsecond",
            Self::Nanosecond => "nanosecond",
        }
    }
}

/// Date and time functions on a temporal column.
pub trait MssqlDate {
    /// The Rust type this column holds.
    type Ty;

    /// The expression a call applies to: the column, or the call before it.
    #[doc(hidden)]
    fn operand(&self) -> stmt::Expr;

    /// The database type the operand's own result takes, so `date_add` answers
    /// the same shape of value the column holds.
    #[doc(hidden)]
    fn stmt_ty() -> stmt::Type;

    /// `YEAR(col)`.
    fn year(&self) -> Expr<i32> {
        call(&self.operand(), stmt::Type::I32, "YEAR", vec![])
    }

    /// `MONTH(col)`.
    fn month(&self) -> Expr<i32> {
        call(&self.operand(), stmt::Type::I32, "MONTH", vec![])
    }

    /// `DAY(col)`.
    fn day(&self) -> Expr<i32> {
        call(&self.operand(), stmt::Type::I32, "DAY", vec![])
    }

    /// `DATEPART(hour, col)`. SQL Server has no `HOUR` function.
    fn hour(&self) -> Expr<i32> {
        call_last(
            &self.operand(),
            stmt::Type::I32,
            "DATEPART",
            vec!["hour".to_owned()],
        )
    }

    /// `DATEPART(minute, col)`. See [`hour`](Self::hour).
    fn minute(&self) -> Expr<i32> {
        call_last(
            &self.operand(),
            stmt::Type::I32,
            "DATEPART",
            vec!["minute".to_owned()],
        )
    }

    /// `DATEPART(second, col)`. See [`hour`](Self::hour).
    fn second(&self) -> Expr<i32> {
        call_last(
            &self.operand(),
            stmt::Type::I32,
            "DATEPART",
            vec!["second".to_owned()],
        )
    }

    /// `DATEPART(part, col)`.
    fn date_part(&self, part: DatePart) -> Expr<i32> {
        call_last(
            &self.operand(),
            stmt::Type::I32,
            "DATEPART",
            vec![part.keyword().to_owned()],
        )
    }

    /// `DATEADD(part, n, col)` — `n` may be negative.
    fn date_add(&self, part: DatePart, n: i64) -> Expr<Self::Ty> {
        call_last(
            &self.operand(),
            Self::stmt_ty(),
            "DATEADD",
            vec![part.keyword().to_owned(), n.to_string()],
        )
    }

    /// `DATETRUNC(part, col)` — SQL Server 2022 and later.
    fn date_trunc(&self, part: DatePart) -> Expr<Self::Ty> {
        call_last(
            &self.operand(),
            Self::stmt_ty(),
            "DATETRUNC",
            vec![part.keyword().to_owned()],
        )
    }

    /// `EOMONTH(col)` — the last day of the column's month.
    fn eomonth(&self) -> Expr<Self::Ty> {
        call(&self.operand(), Self::stmt_ty(), "EOMONTH", vec![])
    }

    /// `EOMONTH(col, n)` — shifted `n` months from the column's month.
    fn eomonth_offset(&self, n: i32) -> Expr<Self::Ty> {
        call(
            &self.operand(),
            Self::stmt_ty(),
            "EOMONTH",
            vec![n.to_string()],
        )
    }
}

macro_rules! mssql_date {
    ($($ty:ty => $stmt_ty:expr;)*) => {
        $(
            impl<Origin> MssqlDate for Path<Origin, $ty> {
                type Ty = $ty;

                fn operand(&self) -> stmt::Expr {
                    Operand::operand(self)
                }

                fn stmt_ty() -> stmt::Type {
                    $stmt_ty
                }
            }

            impl MssqlDate for Expr<$ty> {
                type Ty = $ty;

                fn operand(&self) -> stmt::Expr {
                    Operand::operand(self)
                }

                fn stmt_ty() -> stmt::Type {
                    $stmt_ty
                }
            }
        )*
    };
}

mssql_date! {
    toasty::stmt::Timestamp => stmt::Type::Timestamp;
    toasty::stmt::Date => stmt::Type::Date;
    toasty::stmt::DateTime => stmt::Type::DateTime;
}
