//! JSON functions — Microsoft's [JSON Functions][ref] category.
//!
//! SQL Server has no native JSON column type, so this driver stores a
//! `Vec<scalar>` or `#[document]` value as JSON text in an `NVARCHAR(MAX)`
//! column. That leaves the column with no constraint on it, and makes `ISJSON`
//! the integrity check it otherwise lacks.
//!
//! [ref]: https://learn.microsoft.com/en-us/sql/t-sql/functions/json-functions-transact-sql

use toasty::stmt::{Expr, Path};
use toasty_core::stmt;

use super::{Operand, call, check, text};

/// JSON functions on a text column.
///
/// Every method is usable on a column path *and* on the result of another call,
/// so one function's answer can be fed to the next:
///
/// ```ignore
/// use toasty_driver_mssql_ext::{MssqlJson as _, MssqlStr as _};
///
/// Widget::filter(Widget::fields().notes().is_json_array())
///
/// // `json_query` answers text, and `char_index` reads it.
/// Widget::filter(Widget::fields().notes().json_query("a").char_index("x").gt(0))
/// ```
pub trait MssqlJson {
    /// The expression a call applies to: the column, or the call before it.
    #[doc(hidden)]
    fn operand(&self) -> stmt::Expr;

    /// `ISJSON(col)` — `1` when the column holds a JSON object or array.
    ///
    /// Note that a bare scalar is *not* JSON by this test. Use
    /// [`is_json_value`](Self::is_json_value) for "any JSON at all".
    fn is_json(&self) -> Expr<bool> {
        check(&self.operand(), "ISJSON", vec![])
    }

    /// `ISJSON(col, ARRAY)` — SQL Server 2022 and later.
    fn is_json_array(&self) -> Expr<bool> {
        check(&self.operand(), "ISJSON", vec!["ARRAY".to_owned()])
    }

    /// `ISJSON(col, OBJECT)` — SQL Server 2022 and later.
    fn is_json_object(&self) -> Expr<bool> {
        check(&self.operand(), "ISJSON", vec!["OBJECT".to_owned()])
    }

    /// `ISJSON(col, SCALAR)` — SQL Server 2022 and later.
    fn is_json_scalar(&self) -> Expr<bool> {
        check(&self.operand(), "ISJSON", vec!["SCALAR".to_owned()])
    }

    /// `ISJSON(col, VALUE)` — any JSON value, object or scalar.
    fn is_json_value(&self) -> Expr<bool> {
        check(&self.operand(), "ISJSON", vec!["VALUE".to_owned()])
    }

    /// `JSON_PATH_EXISTS(col, '$.path')` — SQL Server 2022 and later.
    ///
    /// `path` is written without the leading `$.`, so `"a.b"` tests `$.a.b`.
    fn json_path_exists(&self, path: &str) -> Expr<bool> {
        check(&self.operand(), "JSON_PATH_EXISTS", vec![json_path(path)])
    }

    /// `JSON_VALUE(col, '$.path')` — the scalar at `path`, or NULL.
    ///
    /// **This raises rather than returning NULL when the column is not valid
    /// JSON at all** (error 13609). Guarding it with [`is_json`](Self::is_json)
    /// is not reliable, because SQL Server does not promise to evaluate the two
    /// in that order.
    fn json_value(&self, path: &str) -> Expr<String> {
        call(
            &self.operand(),
            stmt::Type::String,
            "JSON_VALUE",
            vec![json_path(path)],
        )
    }

    /// `JSON_QUERY(col, '$.path')` — the object or array at `path`, as text.
    ///
    /// Rejects a *scalar* at `path` with NULL, which is what distinguishes it
    /// from [`json_value`](Self::json_value). It raises on invalid JSON, the
    /// same way `json_value` does.
    fn json_query(&self, path: &str) -> Expr<String> {
        call(
            &self.operand(),
            stmt::Type::String,
            "JSON_QUERY",
            vec![json_path(path)],
        )
    }
}

impl<Origin> MssqlJson for Path<Origin, String> {
    fn operand(&self) -> stmt::Expr {
        Operand::operand(self)
    }
}

impl MssqlJson for Expr<String> {
    fn operand(&self) -> stmt::Expr {
        Operand::operand(self)
    }
}

/// `N'$.a.b'` — the JSON path literal these functions take, from a path written
/// without the leading `$.`.
fn json_path(path: &str) -> String {
    text(&format!("$.{path}"))
}
