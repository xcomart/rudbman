//! Asking a database for a statement's plan instead of running it.
//!
//! Every product has a way, and no two spell it alike. Most put a word in front
//! of the statement and answer with the plan as a result set. Oracle writes the
//! plan into a table and has it read back by a function; SQL Server switches a
//! session option on, answers every statement with its plan while the option
//! stays on, and has to be switched back. [`explain`] writes those statements
//! for one dialect; sending them, and reading the last one's rows, is the
//! caller's.
//!
//! ```
//! use rudbman_sql::{Dialect, explain};
//!
//! let plan = explain(Dialect::POSTGRES, "SELECT * FROM t").unwrap();
//! assert!(plan.before.is_empty() && plan.after.is_empty());
//! assert_eq!(plan.plan, "EXPLAIN SELECT * FROM t");
//!
//! let plan = explain(Dialect::MSSQL, "SELECT * FROM t").unwrap();
//! assert_eq!(plan.before, ["SET SHOWPLAN_TEXT ON"]);
//! assert_eq!(plan.plan, "SELECT * FROM t");
//! assert_eq!(plan.after, ["SET SHOWPLAN_TEXT OFF"]);
//! ```
//!
//! # Nothing here runs the statement
//!
//! No `ANALYZE`, no `EXPLAIN ANALYZE`, no `SET STATISTICS PROFILE`: each of
//! those *executes* the statement to measure it, and a plan of an `UPDATE`
//! asked for in order to see what it would touch must not touch anything.

use crate::dialect::Dialect;

/// The statements that read one statement's plan, in the order they are sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Explain {
    /// Sent first, each on its own, results unread.
    pub before: Vec<String>,
    /// The statement whose result sets are the plan.
    pub plan: String,
    /// Sent last, each on its own, **whatever happened before them** — the
    /// statements that put the session back as it was.
    pub after: Vec<String>,
    /// The statement that says what was asked, for a log line: the plan
    /// statement itself, or the one before it where that is the one that names
    /// the user's statement.
    pub shown: String,
}

impl Explain {
    /// `word` and the statement after it, whose result sets are the plan:
    /// the one-statement form most products answer, with nothing to set up
    /// and nothing to put back.
    pub(crate) fn prefixed(word: &str, sql: &str) -> Self {
        let plan = format!("{word} {sql}");
        Self {
            before: Vec::new(),
            shown: plan.clone(),
            plan,
            after: Vec::new(),
        }
    }
}

/// The statements that read `sql`'s plan on `dialect`, or `None` where this
/// crate does not know how to ask without setting something up first.
///
/// `sql` is one statement, without its terminating semicolon. Each product's
/// file says how it is asked: most prefix the statement with `EXPLAIN`, SQLite
/// with `EXPLAIN QUERY PLAN`, Oracle writes the plan into `PLAN_TABLE` and
/// reads it back, SQL Server switches `SHOWPLAN_TEXT` on and off around it,
/// and Db2 is not asked at all.
pub fn explain(dialect: Dialect, sql: &str) -> Option<Explain> {
    dialect.vendor().explain(sql.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQL: &str = "SELECT * FROM orders WHERE id = 1";

    /// The products that answer one prefixed statement get exactly that, with
    /// nothing to set up and nothing to put back.
    #[test]
    fn a_prefix_is_one_statement() {
        for dialect in [
            Dialect::POSTGRES,
            Dialect::MYSQL,
            Dialect::MARIADB,
            Dialect::H2,
            Dialect::GENERIC,
        ] {
            let plan = explain(dialect, SQL).unwrap();
            assert_eq!(plan.plan, format!("EXPLAIN {SQL}"), "{}", dialect.name());
            assert_eq!(plan.shown, plan.plan);
            assert!(plan.before.is_empty(), "{}", dialect.name());
            assert!(plan.after.is_empty(), "{}", dialect.name());
        }
        assert_eq!(
            explain(Dialect::SQLITE, SQL).unwrap().plan,
            format!("EXPLAIN QUERY PLAN {SQL}")
        );
    }

    /// Oracle's plan is written by one statement and read by another, and the
    /// log names the one that carries the user's statement.
    #[test]
    fn oracle_writes_the_plan_and_reads_it_back() {
        let plan = explain(Dialect::ORACLE, SQL).unwrap();
        assert_eq!(plan.before, [format!("EXPLAIN PLAN FOR {SQL}")]);
        assert_eq!(
            plan.plan,
            "SELECT PLAN_TABLE_OUTPUT FROM TABLE(DBMS_XPLAN.DISPLAY())"
        );
        assert!(plan.after.is_empty());
        assert_eq!(plan.shown, plan.before[0]);
    }

    /// SQL Server's option is switched on before the statement and off after
    /// it, each in a batch of its own as the option requires.
    #[test]
    fn sql_server_switches_the_showplan_option_round_the_statement() {
        let plan = explain(Dialect::MSSQL, SQL).unwrap();
        assert_eq!(plan.before, ["SET SHOWPLAN_TEXT ON"]);
        assert_eq!(plan.plan, SQL);
        assert_eq!(plan.after, ["SET SHOWPLAN_TEXT OFF"]);
    }

    /// Db2 needs explain tables nobody has created, so it is refused rather
    /// than sent something that cannot work.
    #[test]
    fn db2_is_not_asked() {
        assert_eq!(explain(Dialect::DB2, SQL), None);
    }

    /// The statement goes in as it is, trimmed, and nothing in this module
    /// asks the server to execute it.
    #[test]
    fn nothing_is_analyzed() {
        for &dialect in Dialect::ALL {
            // Db2 has no plan to read; see `db2_has_no_plan_to_read`.
            let Some(plan) = explain(dialect, &format!("  {SQL}\n")) else {
                continue;
            };
            for statement in plan.before.iter().chain([&plan.plan]).chain(&plan.after) {
                let upper = statement.to_ascii_uppercase();
                assert!(!upper.contains("ANALYZE"), "{statement}");
                assert!(!upper.contains("STATISTICS"), "{statement}");
                assert!(!statement.ends_with('\n'), "{statement:?}");
            }
        }
    }
}
