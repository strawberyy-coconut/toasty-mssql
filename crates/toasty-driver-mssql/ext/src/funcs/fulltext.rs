//! Full-text predicates — [`CONTAINS`][contains] and [`FREETEXT`][freetext].
//!
//! These search the words in a full-text indexed column rather than matching its
//! text literally, so they are the reason a column needs a full-text index
//! (`CREATE FULLTEXT INDEX`). Without one SQL Server rejects the query with
//! error 7601.
//!
//! A search term is exactly the kind of value that comes from a user, so it is
//! **always bound** — `CONTAINS([body], @p1)` — never written into the statement
//! text. The same holds for the optional `LANGUAGE`.
//!
//! `CONTAINSTABLE` and `FREETEXTTABLE` are table-valued functions, not
//! predicates, and are not covered here.
//!
//! [contains]: https://learn.microsoft.com/en-us/sql/t-sql/queries/contains-transact-sql
//! [freetext]: https://learn.microsoft.com/en-us/sql/t-sql/queries/freetext-transact-sql

use toasty::stmt::{Expr, IntoExpr, Path};
use toasty_core::stmt;

use super::{Arg, Operand, arg, check};

/// Full-text predicates on a text column.
///
/// ```ignore
/// use toasty_driver_mssql_ext::MssqlFullText as _;
///
/// Widget::filter(Widget::fields().body().contains("performance"))
///
/// // The language is bound too.
/// Widget::filter(Widget::fields().body().contains_language("performance", "English"))
/// ```
pub trait MssqlFullText {
    /// The expression a call applies to: the column, or the call before it.
    #[doc(hidden)]
    fn operand(&self) -> stmt::Expr;

    /// `CONTAINS(col, term)` — the column contains the full-text condition
    /// `term`, which may use `AND`, `OR`, `NEAR`, `FORMSOF`, `*` prefixes and
    /// the rest of the `CONTAINS` grammar.
    fn contains(&self, term: impl IntoExpr<String>) -> Expr<bool> {
        check(&self.operand(), "CONTAINS", vec![arg(term)])
    }

    /// `FREETEXT(col, term)` — the column matches the meaning of `term`, which
    /// is word-broken, stemmed and thesaurus-expanded. Unlike `CONTAINS`, no
    /// operator in it is special.
    fn freetext(&self, term: impl IntoExpr<String>) -> Expr<bool> {
        check(&self.operand(), "FREETEXT", vec![arg(term)])
    }

    /// `CONTAINS(col, term, LANGUAGE language)` — the condition is evaluated
    /// with `language`'s word breaker, stemmer and thesaurus.
    ///
    /// `language` is a language *name* such as `"English"` (the `alias` of
    /// `sys.syslanguages`), which is what a bound string can carry.
    fn contains_language(
        &self,
        term: impl IntoExpr<String>,
        language: impl IntoExpr<String>,
    ) -> Expr<bool> {
        check(
            &self.operand(),
            "CONTAINS",
            vec![arg(term), Arg::Keyword("LANGUAGE"), arg(language)],
        )
    }

    /// `FREETEXT(col, term, LANGUAGE language)`. See
    /// [`contains_language`](Self::contains_language).
    fn freetext_language(
        &self,
        term: impl IntoExpr<String>,
        language: impl IntoExpr<String>,
    ) -> Expr<bool> {
        check(
            &self.operand(),
            "FREETEXT",
            vec![arg(term), Arg::Keyword("LANGUAGE"), arg(language)],
        )
    }
}

impl<Origin> MssqlFullText for Path<Origin, String> {
    fn operand(&self) -> stmt::Expr {
        Operand::operand(self)
    }
}

impl MssqlFullText for Expr<String> {
    fn operand(&self) -> stmt::Expr {
        Operand::operand(self)
    }
}
