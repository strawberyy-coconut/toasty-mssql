use super::{Dialect, Formatter, ToSql};

pub(super) struct Ident<S>(pub(super) S);

impl<S: AsRef<str>> ToSql for Ident<S> {
    fn to_sql(self, f: &mut Formatter<'_>) {
        match f.serializer.dialect {
            Dialect::Mysql | Dialect::MariaDb => {
                f.dst.push('`');
                f.dst.push_str(self.0.as_ref());
                f.dst.push('`');
            }
            // T-SQL quotes identifiers with brackets, escaping a literal `]`
            // by doubling it.
            Dialect::Mssql => {
                f.dst.push('[');
                for ch in self.0.as_ref().chars() {
                    if ch == ']' {
                        f.dst.push(']');
                    }
                    f.dst.push(ch);
                }
                f.dst.push(']');
            }
            _ => {
                f.dst.push('"');
                f.dst.push_str(self.0.as_ref());
                f.dst.push('"');
            }
        }
    }
}
