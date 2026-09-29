use super::{Dialect, Ident, ToSql};

use toasty_core::schema::db;

impl ToSql for &db::Type {
    fn to_sql(self, f: &mut super::Formatter<'_>) {
        match self {
            db::Type::Boolean => {
                if f.serializer.is_mssql() {
                    fmt!(f, "BIT")
                } else {
                    fmt!(f, "BOOLEAN")
                }
            }
            db::Type::Integer(1..=2) => fmt!(f, "SMALLINT"),
            db::Type::Integer(3..=4) => {
                if f.serializer.is_mssql() {
                    fmt!(f, "INT")
                } else {
                    fmt!(f, "INTEGER")
                }
            }
            db::Type::Integer(5..=8) => fmt!(f, "BIGINT"),
            db::Type::Integer(_) => todo!(),
            db::Type::UnsignedInteger(size) => {
                match f.serializer.dialect {
                    Dialect::Mysql | Dialect::MariaDb => match size {
                        1 => fmt!(f, "TINYINT UNSIGNED"),
                        2 => fmt!(f, "SMALLINT UNSIGNED"),
                        3..=4 => fmt!(f, "INT UNSIGNED"),
                        5..=8 => fmt!(f, "BIGINT UNSIGNED"),
                        _ => todo!("Unsupported unsigned integer size: {}", size),
                    },
                    Dialect::Postgresql => {
                        match size {
                            1 => fmt!(f, "SMALLINT"),   // u8 -> SMALLINT (i16)
                            2 => fmt!(f, "INTEGER"),    // u16 -> INTEGER (i32)
                            3..=4 => fmt!(f, "BIGINT"), // u32 -> BIGINT (i64)
                            5..=8 => fmt!(f, "BIGINT"), // u64 -> BIGINT (i64) with capability limits
                            _ => todo!("Unsupported unsigned integer size: {}", size),
                        }
                    }
                    Dialect::Sqlite => {
                        // SQLite uses INTEGER for all integer types
                        fmt!(f, "INTEGER")
                    }
                    // T-SQL's integer types are all signed, so an unsigned width
                    // promotes to the next signed width that can hold it. A
                    // 64-bit unsigned value rides `DECIMAL(20, 0)`, which holds
                    // the full `0..=2^64-1` range.
                    Dialect::Mssql => match size {
                        1 => fmt!(f, "SMALLINT"),
                        2 => fmt!(f, "INT"),
                        3..=4 => fmt!(f, "BIGINT"),
                        5..=8 => fmt!(f, "DECIMAL(20, 0)"),
                        _ => todo!("Unsupported unsigned integer size: {}", size),
                    },
                }
            }
            db::Type::Float(size) => match f.serializer.dialect {
                Dialect::Sqlite => fmt!(f, "REAL"),
                Dialect::Postgresql => {
                    if *size <= 4 {
                        fmt!(f, "REAL")
                    } else {
                        fmt!(f, "DOUBLE PRECISION")
                    }
                }
                Dialect::Mysql | Dialect::MariaDb => {
                    if *size <= 4 {
                        fmt!(f, "FLOAT")
                    } else {
                        fmt!(f, "DOUBLE")
                    }
                }
                Dialect::Mssql => {
                    if *size <= 4 {
                        fmt!(f, "REAL")
                    } else {
                        fmt!(f, "FLOAT")
                    }
                }
            },
            // SQL Server has no unbounded `TEXT`; `NVARCHAR(MAX)` is the
            // Unicode equivalent that every collation can store.
            db::Type::Text => {
                if f.serializer.is_mssql() {
                    fmt!(f, "NVARCHAR(MAX)")
                } else {
                    fmt!(f, "TEXT")
                }
            }
            // `NVARCHAR` rather than `VARCHAR` so every string round-trips
            // Unicode without depending on the database's collation.
            db::Type::VarChar(size) => {
                if f.serializer.is_mssql() {
                    fmt!(f, "NVARCHAR(" size ")")
                } else {
                    fmt!(f, "VARCHAR(" size ")")
                }
            }
            db::Type::Uuid => {
                fmt!(
                    f,
                    match f.serializer.dialect {
                        Dialect::Postgresql | Dialect::MariaDb => "UUID",
                        Dialect::Mssql => "UNIQUEIDENTIFIER",
                        _ => todo!("Unsupported type UUID"),
                    }
                );
            }
            db::Type::Numeric(None) => match f.serializer.dialect {
                Dialect::Postgresql => fmt!(f, "NUMERIC"),
                Dialect::Mysql | Dialect::MariaDb => todo!(
                    "MySQL does not support arbitrary-precision NUMERIC; precision and scale must be specified"
                ),
                Dialect::Mssql => todo!(
                    "SQL Server requires an explicit precision and scale for a decimal column"
                ),
                Dialect::Sqlite => todo!("SQLite does not support NUMERIC type"),
            },
            db::Type::Numeric(Some((precision, scale))) => match f.serializer.dialect {
                Dialect::Postgresql => fmt!(f, "NUMERIC(" precision ", " scale ")"),
                Dialect::Mysql | Dialect::MariaDb | Dialect::Mssql => {
                    fmt!(f, "DECIMAL(" precision ", " scale ")")
                }
                Dialect::Sqlite => todo!("SQLite does not support NUMERIC type"),
            },
            db::Type::Binary(size) => match f.serializer.dialect {
                Dialect::Mysql | Dialect::MariaDb | Dialect::Mssql => fmt!(f, "BINARY(" size ")"),
                _ => todo!("Unsupported fixed size binary type"),
            },
            db::Type::Blob => match f.serializer.dialect {
                Dialect::Postgresql => fmt!(f, "BYTEA"),
                Dialect::Mysql | Dialect::MariaDb => fmt!(f, "BLOB"),
                Dialect::Mssql => fmt!(f, "VARBINARY(MAX)"),
                Dialect::Sqlite => fmt!(f, "BLOB"),
            },
            // Not `TIMESTAMP`, which in T-SQL is a synonym for `ROWVERSION`.
            db::Type::Timestamp(precision) => match f.serializer.dialect {
                Dialect::Postgresql => fmt!(f, "TIMESTAMPTZ(" precision ")"),
                Dialect::Mysql | Dialect::MariaDb => fmt!(f, "TIMESTAMP(" precision ")"),
                Dialect::Mssql => fmt!(f, "DATETIME2(" precision ")"),
                Dialect::Sqlite => todo!("SQLite does not support Timestamp"),
            },
            db::Type::Date => match f.serializer.dialect {
                Dialect::Postgresql | Dialect::Mysql | Dialect::MariaDb | Dialect::Mssql => {
                    fmt!(f, "DATE")
                }
                Dialect::Sqlite => todo!("SQLite does not support Date"),
            },
            db::Type::Time(precision) => match f.serializer.dialect {
                Dialect::Postgresql | Dialect::Mysql | Dialect::MariaDb | Dialect::Mssql => {
                    fmt!(f, "TIME(" precision ")")
                }
                Dialect::Sqlite => todo!("SQLite does not support Time"),
            },
            db::Type::DateTime(precision) => match f.serializer.dialect {
                Dialect::Postgresql => fmt!(f, "TIMESTAMP(" precision ")"),
                Dialect::Mysql | Dialect::MariaDb => fmt!(f, "DATETIME(" precision ")"),
                Dialect::Mssql => fmt!(f, "DATETIME2(" precision ")"),
                Dialect::Sqlite => todo!("SQLite does not support DateTime"),
            },
            // SQL Server has no native network address types; the storage layer
            // maps each to the bounded `NVARCHAR` the capability names. These
            // arms are only reachable if that mapping is bypassed, so they
            // mirror the storage widths the capability declares.
            db::Type::Cidr | db::Type::Inet => match f.serializer.dialect {
                Dialect::Postgresql if matches!(self, db::Type::Cidr) => fmt!(f, "CIDR"),
                Dialect::Postgresql => fmt!(f, "INET"),
                Dialect::Mssql => fmt!(f, "NVARCHAR(43)"),
                _ => todo!("Only PostgreSQL supports CIDR/INET"),
            },
            db::Type::MacAddr => match f.serializer.dialect {
                Dialect::Postgresql => fmt!(f, "MACADDR"),
                Dialect::Mssql => fmt!(f, "NVARCHAR(17)"),
                _ => todo!("Only PostgreSQL supports MACADDR"),
            },
            db::Type::MacAddr8 => match f.serializer.dialect {
                Dialect::Postgresql => fmt!(f, "MACADDR8"),
                Dialect::Mssql => fmt!(f, "NVARCHAR(23)"),
                _ => todo!("Only PostgreSQL supports MACADDR8"),
            },
            db::Type::Enum(type_enum) => match f.serializer.dialect {
                // PostgreSQL: reference the named enum type created with CREATE TYPE.
                Dialect::Postgresql => {
                    let name = type_enum
                        .name
                        .as_deref()
                        .expect("PostgreSQL enums require a type name");
                    fmt!(f, Ident(name));
                }
                // MySQL: inline ENUM('label1', 'label2', ...) column type.
                Dialect::Mysql | Dialect::MariaDb => {
                    use toasty_core::stmt::Value;

                    f.dst.push_str("ENUM(");
                    for (i, variant) in type_enum.variants.iter().enumerate() {
                        if i > 0 {
                            f.dst.push_str(", ");
                        }
                        Value::String(variant.name.clone()).to_sql(f);
                    }
                    f.dst.push(')');
                }
                // SQLite: TEXT column (CHECK constraint added in ColumnDef).
                Dialect::Sqlite => fmt!(f, "TEXT"),
                // SQL Server has no native enum type: the variant name lives in
                // a bounded string column pinned by a CHECK constraint (added in
                // `ColumnDef`).
                Dialect::Mssql => fmt!(f, "NVARCHAR(4000)"),
            },
            db::Type::List(elem) => match f.serializer.dialect {
                Dialect::Postgresql => fmt!(f, elem.as_ref() "[]"),
                // MySQL stores `Vec<scalar>` as a JSON document; SQLite uses
                // TEXT (JSON1 functions operate on either, but TEXT is the
                // idiomatic affinity). The element type is tracked by the
                // engine — it doesn't surface in the column DDL.
                Dialect::Mysql | Dialect::MariaDb => fmt!(f, "JSON"),
                Dialect::Sqlite => fmt!(f, "TEXT"),
                // SQL Server has no native array type, so a `Vec<scalar>` is a
                // JSON document in `NVARCHAR(MAX)`.
                Dialect::Mssql => fmt!(f, "NVARCHAR(MAX)"),
            },
            db::Type::Document { binary } => match f.serializer.dialect {
                // `binary` selects `jsonb` over `json` on PostgreSQL; the text
                // encoding (`#[document(text)]`) is not yet wired up.
                Dialect::Postgresql if *binary => fmt!(f, "JSONB"),
                Dialect::Postgresql => fmt!(f, "JSON"),
                Dialect::Mysql | Dialect::MariaDb => fmt!(f, "JSON"),
                Dialect::Sqlite => fmt!(f, "TEXT"),
                // SQL Server has no native JSON type before SQL Server 2025;
                // JSON is stored as `NVARCHAR(MAX)` text.
                Dialect::Mssql => fmt!(f, "NVARCHAR(MAX)"),
            },
            db::Type::Json | db::Type::Jsonb => {
                if f.serializer.is_mssql() {
                    todo!("SQL Server has no native json/jsonb column type")
                }
                match self {
                    db::Type::Json => fmt!(f, "JSON"),
                    _ => fmt!(f, "JSONB"),
                }
            }
            db::Type::Custom(custom) => fmt!(f, custom.as_str()),
        }
    }
}
