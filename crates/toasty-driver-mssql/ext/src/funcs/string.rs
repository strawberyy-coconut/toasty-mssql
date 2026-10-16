//! String functions — Microsoft's [String Functions][ref] category.
//!
//! Every argument that comes from the caller is bound, not inlined; see the
//! [`funcs`](super) module.
//!
//! [ref]: https://learn.microsoft.com/en-us/sql/t-sql/functions/string-functions-transact-sql

use toasty::stmt::{Expr, IntoExpr, Path};
use toasty_core::stmt;

use super::{Operand, arg, call, call_last, check};

/// String functions on a text column.
///
/// Every method is usable on a column path *and* on the result of another call:
///
/// ```ignore
/// use toasty_driver_mssql_ext::MssqlStr as _;
///
/// Widget::filter(Widget::fields().name().len().gt(80))
///
/// // `upper` answers text and `char_index` reads it.
/// Widget::filter(Widget::fields().name().upper().char_index("BOB").eq(1))
/// ```
pub trait MssqlStr {
    /// The expression a call applies to: the column, or the call before it.
    #[doc(hidden)]
    fn operand(&self) -> stmt::Expr;

    /// `LEN(col)` — characters, *ignoring trailing spaces*.
    ///
    /// T-SQL's `LEN` is not `DATALENGTH`. Use
    /// [`datalength`](Self::datalength) for the byte count, which counts
    /// trailing spaces and is doubled for an `NVARCHAR` column.
    fn len(&self) -> Expr<i32> {
        call(&self.operand(), stmt::Type::I32, "LEN", vec![])
    }

    /// `LEN(col) = 0` — true when the column holds no characters, treating
    /// trailing spaces as absent, which is what `LEN` does.
    fn is_empty(&self) -> Expr<bool> {
        self.len().eq(0_i32)
    }

    /// `DATALENGTH(col)` — bytes, so twice the character count for `NVARCHAR`.
    fn datalength(&self) -> Expr<i32> {
        call(&self.operand(), stmt::Type::I32, "DATALENGTH", vec![])
    }

    /// `CHARINDEX(needle, col)` — 1-based position, or `0` when absent.
    ///
    /// Note the argument order: the needle comes first in T-SQL.
    fn char_index(&self, needle: impl IntoExpr<String>) -> Expr<i32> {
        call_last(
            &self.operand(),
            stmt::Type::I32,
            "CHARINDEX",
            vec![arg(needle)],
        )
    }

    /// `PATINDEX(pattern, col)` — like `CHARINDEX` but with `%` and `_`
    /// wildcards, and also taking the pattern first.
    fn pat_index(&self, pattern: impl IntoExpr<String>) -> Expr<i32> {
        call_last(
            &self.operand(),
            stmt::Type::I32,
            "PATINDEX",
            vec![arg(pattern)],
        )
    }

    /// `ISDATE(col)` — `1` when SQL Server can parse the *text* as a date.
    ///
    /// Only offered on a text column: it rejects `datetime2` outright with error
    /// 8116. Microsoft files it under date and time functions, but it accepts
    /// character input only, which is why it is here.
    fn is_date(&self) -> Expr<bool> {
        check(&self.operand(), "ISDATE", vec![])
    }

    /// `ASCII(col)` — the code point of the first character.
    fn ascii(&self) -> Expr<i32> {
        call(&self.operand(), stmt::Type::I32, "ASCII", vec![])
    }

    /// `UNICODE(col)` — the Unicode code point of the first character.
    fn unicode(&self) -> Expr<i32> {
        call(&self.operand(), stmt::Type::I32, "UNICODE", vec![])
    }

    /// `DIFFERENCE(col, other)` — 0 to 4, how alike the two `SOUNDEX`
    /// values are.
    fn difference(&self, other: impl IntoExpr<String>) -> Expr<i32> {
        call(
            &self.operand(),
            stmt::Type::I32,
            "DIFFERENCE",
            vec![arg(other)],
        )
    }

    /// `UPPER(col)`.
    fn upper(&self) -> Expr<String> {
        call(&self.operand(), stmt::Type::String, "UPPER", vec![])
    }

    /// `LOWER(col)`.
    fn lower(&self) -> Expr<String> {
        call(&self.operand(), stmt::Type::String, "LOWER", vec![])
    }

    /// `TRIM(col)` — both ends.
    fn trim(&self) -> Expr<String> {
        call(&self.operand(), stmt::Type::String, "TRIM", vec![])
    }

    /// `LTRIM(col)`.
    fn ltrim(&self) -> Expr<String> {
        call(&self.operand(), stmt::Type::String, "LTRIM", vec![])
    }

    /// `RTRIM(col)`.
    fn rtrim(&self) -> Expr<String> {
        call(&self.operand(), stmt::Type::String, "RTRIM", vec![])
    }

    /// `REVERSE(col)`.
    fn reverse(&self) -> Expr<String> {
        call(&self.operand(), stmt::Type::String, "REVERSE", vec![])
    }

    /// `SOUNDEX(col)`.
    fn soundex(&self) -> Expr<String> {
        call(&self.operand(), stmt::Type::String, "SOUNDEX", vec![])
    }

    /// `REPLACE(col, from, to)`.
    fn replace(&self, from: impl IntoExpr<String>, to: impl IntoExpr<String>) -> Expr<String> {
        call(
            &self.operand(),
            stmt::Type::String,
            "REPLACE",
            vec![arg(from), arg(to)],
        )
    }

    /// `SUBSTRING(col, start, length)` — 1-based `start`.
    ///
    /// T-SQL's `SUBSTRING` requires all three arguments.
    fn substring(&self, start: impl IntoExpr<i32>, length: impl IntoExpr<i32>) -> Expr<String> {
        call(
            &self.operand(),
            stmt::Type::String,
            "SUBSTRING",
            vec![arg(start), arg(length)],
        )
    }

    /// `LEFT(col, n)`.
    fn left(&self, n: impl IntoExpr<i32>) -> Expr<String> {
        call(&self.operand(), stmt::Type::String, "LEFT", vec![arg(n)])
    }

    /// `RIGHT(col, n)`.
    fn right(&self, n: impl IntoExpr<i32>) -> Expr<String> {
        call(&self.operand(), stmt::Type::String, "RIGHT", vec![arg(n)])
    }

    /// `REPLICATE(col, n)`.
    fn replicate(&self, n: impl IntoExpr<i32>) -> Expr<String> {
        call(
            &self.operand(),
            stmt::Type::String,
            "REPLICATE",
            vec![arg(n)],
        )
    }
}

impl<Origin> MssqlStr for Path<Origin, String> {
    fn operand(&self) -> stmt::Expr {
        Operand::operand(self)
    }
}

impl MssqlStr for Expr<String> {
    fn operand(&self) -> stmt::Expr {
        Operand::operand(self)
    }
}
