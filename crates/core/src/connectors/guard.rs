// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! App-level read-only guard (validation answer #6, defense-in-depth behind
//! the compiler): only a single SELECT statement reaches any driver, and it
//! must be one statement (no `;` chains, no comments smuggling).

use querora_contracts::{ErrorCode, ToolError};
use sqlparser::ast::{SetExpr, Statement};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

/// Validate that `sql` is a single read-only SELECT (with optional CTEs and
/// nested parens). Rejects everything else, including trailing semicolons
/// that would let a second statement hide behind the first.
pub fn assert_single_select(sql: &str) -> Result<String, ToolError> {
    let stmts = Parser::parse_sql(&GenericDialect {}, sql)
        .map_err(|e| ToolError::new(ErrorCode::InvalidIr, format!("SQL parse failed: {e}")))?;

    if stmts.len() != 1 {
        return Err(ToolError::new(
            ErrorCode::InvalidIr,
            format!("exactly one statement allowed, found {}", stmts.len()),
        ));
    }
    match &stmts[0] {
        Statement::Query(q) => {
            // unwrap nested parens/CTEs to the core SELECT body
            let mut body: &SetExpr = &q.body;
            loop {
                match body {
                    SetExpr::Query(inner) => body = &inner.body,
                    SetExpr::Select(_) => break,
                    other => {
                        return Err(ToolError::new(
                            ErrorCode::InvalidIr,
                            format!("read-only guard: unsupported query body `{other}`"),
                        ))
                    }
                }
            }
            // reject locking reads (FOR UPDATE etc.)
            if !q.locks.is_empty() {
                return Err(ToolError::new(
                    ErrorCode::InvalidIr,
                    "locking reads are not allowed",
                ));
            }
            Ok(sql.to_string())
        }
        other => Err(ToolError::new(
            ErrorCode::InvalidIr,
            format!("read-only guard: only SELECT statements are allowed, got `{other}`"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_plain_select() {
        assert!(assert_single_select("SELECT a, b FROM t WHERE x = $1 GROUP BY a").is_ok());
    }

    #[test]
    fn allows_cte_select() {
        assert!(assert_single_select("WITH m AS (SELECT 1 AS x) SELECT * FROM m").is_ok());
    }

    #[test]
    fn allows_nested_parens() {
        assert!(assert_single_select("(SELECT 1)").is_ok());
        assert!(assert_single_select("((SELECT 1))").is_ok());
    }

    #[test]
    fn rejects_multi_statement() {
        assert!(assert_single_select("SELECT 1; SELECT 2").is_err());
        assert!(assert_single_select("SELECT 1;DROP TABLE t").is_err());
        // a single trailing semicolon parses as one statement — safe
        assert!(assert_single_select("SELECT 1;").is_ok());
    }

    #[test]
    fn rejects_writes_and_ddl() {
        for sql in [
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "DROP TABLE t",
            "CREATE TABLE t (a int)",
            "ALTER TABLE t ADD COLUMN b int",
            "TRUNCATE TABLE t",
            "VACUUM",
        ] {
            assert!(assert_single_select(sql).is_err(), "must reject: {sql}");
        }
    }

    #[test]
    fn rejects_union_bodies_and_locking() {
        // UNION at top level is not a plain Select body — rejected for M0
        assert!(assert_single_select("SELECT 1 UNION SELECT 2").is_err());
        assert!(assert_single_select("SELECT 1 FOR UPDATE").is_err());
    }
}
