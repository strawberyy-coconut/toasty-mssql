//! Migration support that is specific to the TDS client.
//!
//! Toasty diffs two schemas and renders the diff to T-SQL through
//! [`toasty_sql`], so what is left here is the part that depends on how a SQL
//! Server batch is sent: splitting a migration into the batches the client can
//! execute, and turning an unrenderable diff into SQL that raises.

use toasty_core::Error;

/// The marker [`toasty_core::schema::db::Migration::new_sql_with_breakpoints`]
/// separates statements with, and that [`batches`] splits on.
pub(crate) const BREAKPOINT: &str = "-- #[toasty::breakpoint]";

/// Splits a migration into the batches that can be sent to the server.
///
/// Two separators apply, and neither is a semicolon:
///
/// * [`BREAKPOINT`], which this driver's own `generate_migration` writes
///   between statements. `;` cannot be used because it can appear inside a
///   string literal or a trigger body, and splitting on it would cut a
///   statement in half.
/// * `GO` on a line of its own, which is how SQL Server tooling separates
///   batches and which a hand-written migration will therefore contain. `GO` is
///   not T-SQL — it is interpreted by the client — so sending it to the server
///   fails with "Could not find stored procedure 'GO'".
pub(crate) fn batches(sql: &str) -> Vec<String> {
    let mut batches: Vec<String> = Vec::new();
    let mut current = String::new();

    let flush = |current: &mut String, batches: &mut Vec<String>| {
        let batch = current.trim();
        if !batch.is_empty() {
            batches.push(batch.to_owned());
        }
        current.clear();
    };

    for statement in sql.split(BREAKPOINT) {
        for line in statement.lines() {
            if line.trim().eq_ignore_ascii_case("GO") {
                flush(&mut current, &mut batches);
            } else {
                current.push_str(line);
                current.push('\n');
            }
        }

        // A breakpoint separates statements, so it ends the current batch even
        // when the next one does not open with `GO`.
        flush(&mut current, &mut batches);
    }

    batches
}

/// A migration that fails when it is applied.
///
/// `Driver::generate_migration` cannot return an error, and applying a comment
/// would look like success and be recorded as such, so an unrenderable diff
/// becomes SQL that raises with the reason instead.
pub(crate) fn unrenderable(error: &Error) -> String {
    let reason = error.to_string().replace('\n', " ").replace('\'', "''");

    format!("-- The SQL Server driver cannot render this migration.\nTHROW 50000, N'{reason}', 1;")
}
