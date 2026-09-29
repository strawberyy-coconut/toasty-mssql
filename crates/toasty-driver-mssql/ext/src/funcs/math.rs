//! Mathematical functions — Microsoft's [Mathematical Functions][ref] category.
//!
//! [ref]: https://learn.microsoft.com/en-us/sql/t-sql/functions/mathematical-functions-transact-sql

use toasty::stmt::{Expr, Path};
use toasty_core::stmt;

use super::{Operand, call};

/// Mathematical functions on a numeric column.
///
/// `sqrt`, `power`, `log`, `log10` and `exp` answer a `float` whatever the
/// column holds, because that is what T-SQL returns for them.
pub trait MssqlMath {
    /// The Rust type this column holds.
    type Ty;

    /// The expression a call applies to: the column, or the call before it.
    #[doc(hidden)]
    fn operand(&self) -> stmt::Expr;

    /// The database type the operand's own result takes, so a chained call is
    /// typed as the column was rather than as its predecessor's answer.
    #[doc(hidden)]
    fn stmt_ty() -> stmt::Type;

    /// `ABS(col)`.
    fn abs(&self) -> Expr<Self::Ty> {
        call(&self.operand(), Self::stmt_ty(), "ABS", vec![])
    }

    /// `SIGN(col)` — `-1`, `0` or `1`.
    fn sign(&self) -> Expr<Self::Ty> {
        call(&self.operand(), Self::stmt_ty(), "SIGN", vec![])
    }

    /// `SQUARE(col)`.
    fn square(&self) -> Expr<Self::Ty> {
        call(&self.operand(), Self::stmt_ty(), "SQUARE", vec![])
    }

    /// `CEILING(col)`.
    fn ceiling(&self) -> Expr<Self::Ty> {
        call(&self.operand(), Self::stmt_ty(), "CEILING", vec![])
    }

    /// `FLOOR(col)`.
    fn floor(&self) -> Expr<Self::Ty> {
        call(&self.operand(), Self::stmt_ty(), "FLOOR", vec![])
    }

    /// `ROUND(col, digits)`.
    fn round(&self, digits: i32) -> Expr<Self::Ty> {
        call(
            &self.operand(),
            Self::stmt_ty(),
            "ROUND",
            vec![digits.to_string()],
        )
    }

    /// `SQRT(col)`.
    ///
    /// A negative input is an error, not NULL: SQL Server raises "An invalid
    /// floating point operation occurred", which fails the whole query rather
    /// than the row.
    fn sqrt(&self) -> Expr<f64> {
        call(&self.operand(), stmt::Type::F64, "SQRT", vec![])
    }

    /// `POWER(col, n)`.
    fn power(&self, n: f64) -> Expr<f64> {
        call(
            &self.operand(),
            stmt::Type::F64,
            "POWER",
            vec![n.to_string()],
        )
    }

    /// `LOG(col)` — natural logarithm. Zero and negative inputs raise.
    fn log(&self) -> Expr<f64> {
        call(&self.operand(), stmt::Type::F64, "LOG", vec![])
    }

    /// `LOG10(col)`. Zero and negative inputs raise.
    fn log10(&self) -> Expr<f64> {
        call(&self.operand(), stmt::Type::F64, "LOG10", vec![])
    }

    /// `EXP(col)`.
    fn exp(&self) -> Expr<f64> {
        call(&self.operand(), stmt::Type::F64, "EXP", vec![])
    }
}

macro_rules! mssql_math {
    ($($ty:ty => $stmt_ty:expr;)*) => {
        $(
            impl<Origin> MssqlMath for Path<Origin, $ty> {
                type Ty = $ty;

                fn operand(&self) -> stmt::Expr {
                    Operand::operand(self)
                }

                fn stmt_ty() -> stmt::Type {
                    $stmt_ty
                }
            }

            impl MssqlMath for Expr<$ty> {
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

mssql_math! {
    i32 => stmt::Type::I32;
    i64 => stmt::Type::I64;
    f32 => stmt::Type::F32;
    f64 => stmt::Type::F64;
}
