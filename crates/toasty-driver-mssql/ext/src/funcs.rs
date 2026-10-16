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
//!     base: Expr::Record([ <operand>, <arg₁>, <arg₂>, … ]),
//!     // or, when the call takes no expression arguments, the bare operand.
//!     path: ["!ISJSON", "{base}", "ARRAY"],     // name, then the arguments
//!     ty:   <the result type>,
//! }
//! ```
//!
//! * `!` marks the path as a call rather than a document path extraction. It
//!   cannot collide with one, because those are built from Rust field names and
//!   `!` is not permitted in an identifier.
//! * `{base}` is the operand, `{base:N}` is field `N` of the `Record` base. The
//!   renderer substitutes the corresponding expression, so a value placed there
//!   is a real bind parameter — `CONTAINS([body], @p1)`, not `CONTAINS([body],
//!   N'…')`.
//! * A `%keyword` token (`%LANGUAGE`) opens a clause: it is emitted after a
//!   comma and the argument that follows it is separated by a space, giving
//!   `CONTAINS([body], @p1, LANGUAGE @p2)`.
//! * `!.` marks a call that is a *method* on the operand, which is how T-SQL
//!   spells its whole spatial vocabulary: `col.STArea()`.
//!
//! Because `base` is an arbitrary expression and the renderer walks it
//! recursively, a call can also be the receiver of another — which is what makes
//! `col.STUnion(…).STArea()` expressible.
//!
//! # Arguments are bound, not inlined
//!
//! Every argument the caller supplies — the search term, a substring length, a
//! well-known text geometry — is carried as an expression in the `Record` base
//! and reaches the database as a bind parameter. Only fixed T-SQL *keywords*
//! (the `ARRAY` of `ISJSON`, the `day` of `DATEPART`) are written into the SQL,
//! and those come from Rust enums rather than from the caller. This removes the
//! injection footgun an inlined string would be: no API writes a caller's text
//! into the statement. The one inline escape hatch, [`lit`], takes numeric types
//! only and so cannot carry a quote.
//!
//! # What this cannot do
//!
//! * **One operand, then expression arguments.** The `Record` base holds the
//!   operand and the call's bindable arguments, so a call can take several. Any
//!   argument that is not one of those two kinds travels as inline text in
//!   `path`, which is how keywords are written.
//! * **Filters only.** The engine drops a non-path expression in a projection,
//!   so these work in `WHERE` and nowhere a value is selected.
//!
//! Both limitations disappear with an upstream
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

    /// One argument of a carried call.
    pub(crate) enum Arg {
        /// The operand, substituted wherever it appears in the argument list.
        Operand,
        /// A bindable expression.
        Expr(stmt::Expr),
        /// An inline literal token, such as an enum keyword (`day`) or `ARRAY`.
        Raw(String),
        /// A keyword that introduces a clause whose value follows it: emitted
        /// after a comma, with the next argument separated by a space.
        Keyword(&'static str),
    }

    impl From<&'static str> for Arg {
        fn from(value: &'static str) -> Self {
            Arg::Raw(value.to_owned())
        }
    }

    impl From<String> for Arg {
        fn from(value: String) -> Self {
            Arg::Raw(value)
        }
    }

    /// A bindable expression argument, from anything that converts to an
    /// `Expr<T>`.
    pub(crate) fn arg<T>(value: impl IntoExpr<T>) -> Arg {
        Arg::Expr(Operand::operand(&value.into_expr()))
    }

    /// Encodes a call as a `FuncJsonExtract`.
    ///
    /// `operand` becomes field 0 of the `Record` base and each [`Arg::Expr`]
    /// follows it; `args` gives the final T-SQL argument order, so the operand
    /// token may sit anywhere in it. A call with no expression arguments keeps
    /// the bare operand as `base`, so its wire form is unchanged.
    fn encode(
        operand: stmt::Expr,
        ty: stmt::Type,
        prefix: &str,
        name: &str,
        args: Vec<Arg>,
    ) -> stmt::Expr {
        let mut fields = vec![operand];
        let mut path = Vec::with_capacity(args.len() + 1);
        path.push(format!("{MARKER}{prefix}{name}"));

        for arg in args {
            match arg {
                Arg::Operand => path.push(OPERAND.to_owned()),
                Arg::Expr(expr) => {
                    let index = fields.len();
                    fields.push(expr);
                    path.push(format!("{{base:{index}}}"));
                }
                Arg::Raw(raw) => path.push(raw),
                Arg::Keyword(keyword) => path.push(format!("%{keyword}")),
            }
        }

        let base = if fields.len() == 1 {
            fields.pop().expect("the operand is always present")
        } else {
            stmt::Expr::record(fields)
        };

        stmt::Expr::Func(stmt::ExprFunc::JsonExtract(stmt::FuncJsonExtract {
            base: Box::new(base),
            path,
            ty,
        }))
    }

    /// `expr = true` — the shape the engine's boolean-position check accepts for
    /// a call that answers a predicate. See [`crate::funcs`] for why the
    /// right-hand side is a schema-fixed `Static` rather than a bound value.
    fn truthy(expr: stmt::Expr) -> stmt::Expr {
        stmt::Expr::binary_op(
            expr,
            stmt::BinaryOp::Eq,
            stmt::Expr::Static(stmt::Value::Bool(true)),
        )
    }

    /// `name(operand[, args…])`, tagged `T`.
    pub(crate) fn call<T, O: Operand>(
        base: &O,
        ty: stmt::Type,
        name: &str,
        mut args: Vec<Arg>,
    ) -> Expr<T> {
        args.insert(0, Arg::Operand);
        Expr::from_untyped(encode(base.operand(), ty, "", name, args))
    }

    /// `name([args…,] operand)`, for the functions that take their subject last.
    pub(crate) fn call_last<T, O: Operand>(
        base: &O,
        ty: stmt::Type,
        name: &str,
        mut args: Vec<Arg>,
    ) -> Expr<T> {
        args.push(Arg::Operand);
        Expr::from_untyped(encode(base.operand(), ty, "", name, args))
    }

    /// `name(operand[, args…]) = true`, for functions that answer yes or no.
    pub(crate) fn check<O: Operand>(base: &O, name: &str, mut args: Vec<Arg>) -> Expr<bool> {
        args.insert(0, Arg::Operand);
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
        args: Vec<Arg>,
    ) -> Expr<T> {
        Expr::from_untyped(encode(base.operand(), ty, &METHOD.to_string(), name, args))
    }

    /// `operand.name(args…) = true`.
    pub(crate) fn method_check<O: Operand>(base: &O, name: &str, args: Vec<Arg>) -> Expr<bool> {
        Expr::from_untyped(truthy(encode(
            base.operand(),
            stmt::Type::Bool,
            &METHOD.to_string(),
            name,
            args,
        )))
    }

    /// `geometry::STGeomFromText(wkt, srid)` — a schema-qualified constructor
    /// call, as an argument. Both the well-known text and the SRID are bound.
    pub(crate) fn constructor_arg(
        name: &str,
        wkt: impl IntoExpr<String>,
        srid: impl IntoExpr<i32>,
    ) -> Arg {
        let wkt = Operand::operand(&wkt.into_expr());
        let args = vec![Arg::Operand, arg(srid)];
        Arg::Expr(encode(wkt, stmt::Type::Bytes, "", name, args))
    }
}

// No single category uses all of these, so which ones are unused depends on
// which features are on. The builder module above is where that is settled; this
// just brings them up to the modules that do use them.
#[allow(unused_imports)]
#[cfg(feature = "toasty")]
pub(crate) use build::{
    Arg, Operand, arg, call, call_last, check, constructor_arg, method, method_check,
};

#[cfg(feature = "funcs-date")]
mod date;
#[cfg(feature = "funcs-fulltext")]
mod fulltext;
#[cfg(feature = "funcs-json")]
mod json;
#[cfg(feature = "funcs-math")]
mod math;
#[cfg(feature = "funcs-string")]
mod string;

#[cfg(feature = "funcs-date")]
pub use date::{DatePart, MssqlDate};
#[cfg(feature = "funcs-fulltext")]
pub use fulltext::MssqlFullText;
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
