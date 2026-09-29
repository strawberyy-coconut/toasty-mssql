//! SQL Server scalar functions, callable from a Toasty query filter.
//!
//! Toasty's expression AST has no variant for "call a function by name", and it
//! is a closed enum, so a driver cannot add one. A call is therefore carried in
//! the node Toasty uses for `#[document]` paths,
//! [`FuncJsonExtract`](toasty_core::stmt::FuncJsonExtract), which holds *both* an
//! arbitrary operand expression (`base`) and a `Vec<String>` (`path`):
//!
//! ```text
//! FuncJsonExtract {
//!     base: <the column expression>,           // rendered as tbl_0_0.[name]
//!     path: ["!ISJSON", "{base}", "ARRAY"],    // name, then the arguments
//!     ty: <the result type>,
//! }
//! ```
//!
//! * `!` marks the path as a call rather than a document path extraction. It
//!   cannot collide with one, because those are built from Rust field names and
//!   `!` is not permitted in an identifier.
//! * `{base}` marks *where* the operand goes, because T-SQL is not consistent
//!   about it: `ISJSON(col, ARRAY)` takes its subject first, `DATEPART(day, col)`
//!   and `CHARINDEX(N'a', col)` take it last.
//! * `!.` marks a call that is a *method* on the operand, which is how T-SQL
//!   spells its whole spatial vocabulary: `col.STArea()`.
//!
//! Because `base` is an arbitrary expression and the renderer walks it
//! recursively, a call can also be the receiver of another — which is what makes
//! `col.STUnion(…).STArea()` expressible.
//!
//! The functions are grouped the way Microsoft groups them, one Cargo feature
//! per category: `funcs-json`, `funcs-string`, `funcs-math` and `funcs-date`,
//! with `funcs` turning on all four. Spatial methods ride with the `spatial`
//! feature, because they need the types it defines.
//!
//! # What this cannot do
//!
//! * **One expression operand.** A `FuncJsonExtract` has a single `base`, so
//!   `CHARINDEX` can take a column and a literal, but not two columns. The
//!   remaining arguments travel as escaped text in `path`.
//! * **No bind parameters inside the call.** Every argument is inlined, so no
//!   argument may be untrusted input.
//! * **Filters only.** The engine drops a non-path expression in a projection,
//!   so these work in `WHERE` and nowhere a value is selected.
//!
//! All three limitations disappear with an upstream
//! `ExprFunc::Custom(FuncCustom { name, args, ret })`, which is the real fix.

/// The wire-format markers that carry a call in a `FuncJsonExtract` path.
///
/// [`toasty_sql`] decodes them when it renders the expression, so the format
/// has a single definition there and the builders here share it.
#[cfg(feature = "toasty")]
pub(crate) use toasty_sql::serializer::{
    MSSQL_FUNC_MARKER as MARKER, MSSQL_FUNC_METHOD as METHOD, MSSQL_FUNC_OPERAND as OPERAND,
};

#[cfg(feature = "toasty")]
use toasty::stmt::Expr;

/// The builders the categories share.
///
/// Every one of these is used by at least one category, but none is used by
/// *all* of them — so a build with only `funcs-json` leaves the rest
/// unreachable, and which ones are unreachable depends on which features are on.
/// The module is private and these are `pub(crate)`, so the dead-code lint is
/// the only thing that could complain; it is silenced once here rather than
/// tracked per helper against every combination of features.
#[cfg(feature = "toasty")]
#[allow(dead_code)]
mod build {
    use toasty::stmt::{Expr, IntoExpr, Path};
    use toasty_core::stmt;

    use super::{MARKER, METHOD, OPERAND};

    /// What a call applies to: a column path, or the result of another call.
    ///
    /// The expression impl is what lets a call be the *receiver* of another —
    /// `col.STUnion(…).STArea()` — because the carrier already holds an
    /// arbitrary expression in `base`, and the renderer already renders it
    /// recursively.
    pub(crate) trait Operand {
        /// The expression the operand renders as.
        fn operand(&self) -> stmt::Expr;
    }

    impl<Origin, U> Operand for Path<Origin, U> {
        fn operand(&self) -> stmt::Expr {
            stmt::Expr::from(self.clone().into_expr())
        }
    }

    impl<U> Operand for Expr<U> {
        fn operand(&self) -> stmt::Expr {
            stmt::Expr::from(self.clone())
        }
    }

    impl Operand for stmt::Expr {
        fn operand(&self) -> stmt::Expr {
            self.clone()
        }
    }

    /// Encodes a call as a `FuncJsonExtract`.
    ///
    /// `prefix` is what goes between the marker and the name — empty for a
    /// function, [`METHOD`] for a method — and `args` must already carry the
    /// [`OPERAND`] token if the operand is one of them.
    pub(crate) fn encode(
        base: stmt::Expr,
        ty: stmt::Type,
        prefix: &str,
        name: &str,
        args: Vec<String>,
    ) -> stmt::Expr {
        let mut path = Vec::with_capacity(args.len() + 1);
        path.push(format!("{MARKER}{prefix}{name}"));
        path.extend(args);

        stmt::Expr::Func(stmt::ExprFunc::JsonExtract(stmt::FuncJsonExtract {
            base: Box::new(base),
            path,
            ty,
        }))
    }

    /// A T-SQL string literal: `N`-prefixed, with `'` doubled.
    pub(crate) fn text(value: &str) -> String {
        format!("N'{}'", value.replace('\'', "''"))
    }

    /// `expr = 1` — T-SQL has no boolean expression type, and the comparison is
    /// also the shape the engine's boolean-position check accepts.
    pub(crate) fn truthy(expr: stmt::Expr) -> stmt::Expr {
        stmt::Expr::binary_op(
            expr,
            stmt::BinaryOp::Eq,
            stmt::Expr::Value(stmt::Value::Bool(true)),
        )
    }

    /// `name(operand[, args…])`, tagged `T`.
    pub(crate) fn call<T, O: Operand>(
        base: &O,
        ty: stmt::Type,
        name: &str,
        args: Vec<String>,
    ) -> Expr<T> {
        let mut args = args;
        args.insert(0, OPERAND.to_owned());

        Expr::from_untyped(encode(base.operand(), ty, "", name, args))
    }

    /// `name([args…,] operand)`, for the functions that take their subject last.
    pub(crate) fn call_last<T, O: Operand>(
        base: &O,
        ty: stmt::Type,
        name: &str,
        mut args: Vec<String>,
    ) -> Expr<T> {
        args.push(OPERAND.to_owned());

        Expr::from_untyped(encode(base.operand(), ty, "", name, args))
    }

    /// `name(operand[, args…]) = 1`, for functions that answer yes or no.
    pub(crate) fn check<O: Operand>(base: &O, name: &str, args: Vec<String>) -> Expr<bool> {
        let mut args = args;
        args.insert(0, OPERAND.to_owned());

        Expr::from_untyped(truthy(encode(
            base.operand(),
            stmt::Type::Bool,
            "",
            name,
            args,
        )))
    }

    /// `operand.name(args…)`, for a call that is a method rather than a function.
    pub(crate) fn method<T, O: Operand>(
        base: &O,
        ty: stmt::Type,
        name: &str,
        args: Vec<String>,
    ) -> Expr<T> {
        Expr::from_untyped(encode(base.operand(), ty, &METHOD.to_string(), name, args))
    }

    /// `operand.name(args…) = 1`.
    pub(crate) fn method_check<O: Operand>(base: &O, name: &str, args: Vec<String>) -> Expr<bool> {
        Expr::from_untyped(truthy(encode(
            base.operand(),
            stmt::Type::Bool,
            &METHOD.to_string(),
            name,
            args,
        )))
    }
}

// No single category uses all of these, so which ones are unused depends on
// which features are on. The builder module above is where that is settled; this
// just brings them up to the modules that do use them.
#[allow(unused_imports)]
#[cfg(feature = "toasty")]
pub(crate) use build::{Operand, call, call_last, check, method, method_check, text};

#[cfg(feature = "funcs-date")]
mod date;
#[cfg(feature = "funcs-json")]
mod json;
#[cfg(feature = "funcs-math")]
mod math;
#[cfg(feature = "funcs-string")]
mod string;

#[cfg(feature = "funcs-date")]
pub use date::{DatePart, MssqlDate};
#[cfg(feature = "funcs-json")]
pub use json::MssqlJson;
#[cfg(feature = "funcs-math")]
pub use math::MssqlMath;
#[cfg(feature = "funcs-string")]
pub use string::MssqlStr;

/// A numeric constant that can be written into the SQL rather than bound.
///
/// Implementing this for your own type is a matter of producing a T-SQL numeric
/// literal; see [`lit`] for why it is needed at all.
#[cfg(feature = "toasty")]
pub trait MssqlLiteral {
    /// This value as T-SQL numeric text.
    fn sql_text(self) -> String;
}

#[cfg(feature = "toasty")]
macro_rules! mssql_literal {
    ($($ty:ty;)*) => {
        $(
            impl MssqlLiteral for $ty {
                fn sql_text(self) -> String {
                    self.to_string()
                }
            }
        )*
    };
}

#[cfg(feature = "toasty")]
mssql_literal! {
    i8; i16; i32; i64; u8; u16; u32; u64; f32; f64;
}

/// A numeric constant written into the SQL text instead of being bound.
///
/// Needed because the engine cannot type a parameter that sits opposite a
/// function call: Toasty's bind pass infers a parameter's type from the
/// expression on the other side of the comparison, and it has no rule for
/// [`ExprFunc`](toasty_core::stmt::ExprFunc). The type must therefore come from
/// the value itself, which it does for most types but not for `f32`, `f64`,
/// `BigDecimal` or `Zoned`.
///
/// So this is a panic:
///
/// ```ignore
/// Widget::filter(Widget::fields().price().sqrt().lt(4.0));   // panics
/// ```
///
/// and this is the same comparison with the constant inlined:
///
/// ```ignore
/// Widget::filter(Widget::fields().price().sqrt().lt(lit(4.0)));
/// ```
///
/// Only numeric types are accepted, because their text form cannot carry a
/// quote and so cannot escape into the surrounding statement. `NaN` and the
/// infinities have no T-SQL literal and must not be passed.
#[cfg(feature = "toasty")]
pub fn lit<T: MssqlLiteral>(value: T) -> Expr<T> {
    Expr::from_untyped(toasty_core::stmt::Expr::Ident(value.sql_text()))
}

#[cfg(all(test, feature = "toasty"))]
mod tests {
    use super::*;

    #[test]
    fn escapes_a_string_literal() {
        assert_eq!(build::text("plain"), "N'plain'");
        assert_eq!(build::text("o'brien"), "N'o''brien'");
        assert_eq!(build::text("'; DROP TABLE x --"), "N'''; DROP TABLE x --'");
    }
}
