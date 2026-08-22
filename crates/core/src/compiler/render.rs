// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Dialect rendering: logical planned SQL → final dialect SQL.
//! Quoting, time-grain functions, and placeholder style per dialect.
//! Golden files (`tests/golden/*.sql`) lock each dialect's shape.

use super::plan::{ExecutionPlan, ResolvedQuery};
use super::validate::TIMEOUT_SECS;
use crate::connectors::types::Dialect;
use querora_contracts::{TimeGrain, ToolError};

/// Render the planned query for `dialect` into an executable plan.
pub fn render(p: &ResolvedQuery, dialect: Dialect) -> Result<ExecutionPlan, ToolError> {
    let quote = quote_fn(dialect);
    let grain_fn = grain_expr_fn(dialect);

    let mut select_parts: Vec<String> = Vec::new();
    for item in &p.select {
        let expr = materialize(&item.expr, &quote, &grain_fn);
        select_parts.push(format!("{expr} AS {}", quote(&item.alias)));
    }
    let from = format!("{} {}", quote(&p.base_table), quote(&p.base_alias));
    let mut joins = String::new();
    for j in &p.joins {
        joins.push_str(&format!(
            " LEFT JOIN {} {} ON {} = {}",
            quote(&j.table),
            quote(&j.alias),
            materialize(&j.on_left, &quote, &grain_fn),
            materialize(&j.on_right, &quote, &grain_fn),
        ));
    }
    let mut where_parts: Vec<String> = Vec::new();
    let mut params: Vec<serde_json::Value> = Vec::new();
    for c in &p.conditions {
        where_parts.push(materialize(&c.sql, &quote, &grain_fn));
        params.extend(c.params.iter().cloned());
    }
    let group_by: Vec<String> = p.group_by.iter().map(|a| quote(a)).collect();
    let order_by: Vec<String> = p
        .order
        .iter()
        .map(|(a, d)| format!("{} {d}", quote(a)))
        .collect();

    // virtual-entity CTEs (definitions already validated single-SELECT)
    let mut sql = if p.ctes.is_empty() {
        format!("SELECT {} FROM {from}", select_parts.join(", "))
    } else {
        let with: Vec<String> = p
            .ctes
            .iter()
            .map(|(name, def)| format!("{} AS ({})", quote(name), def))
            .collect();
        format!(
            "WITH {} SELECT {} FROM {from}",
            with.join(", "),
            select_parts.join(", ")
        )
    };
    sql.push_str(&joins);
    if !where_parts.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_parts.join(" AND "));
    }
    if !group_by.is_empty() {
        sql.push_str(" GROUP BY ");
        sql.push_str(&group_by.join(", "));
    }
    if !order_by.is_empty() {
        sql.push_str(" ORDER BY ");
        sql.push_str(&order_by.join(", "));
    }
    sql.push_str(&format!(" LIMIT {}", p.limit));

    // Postgres uses $n placeholders; rewrite the ?-shaped SQL in order.
    if dialect == Dialect::Pg {
        let mut n = 0;
        let mut out = String::with_capacity(sql.len() + 8);
        for c in sql.chars() {
            if c == '?' {
                n += 1;
                out.push_str(&format!("${n}"));
            } else {
                out.push(c);
            }
        }
        sql = out;
    }

    Ok(ExecutionPlan {
        sql,
        params,
        timeout_secs: TIMEOUT_SECS,
        row_cap: p.limit,
        semantic_version: String::new(), // stamped by execute_query at exec time
        resolved_time_bounds: p.resolved_time_bounds.clone(),
        explain_only: false,
    })
}

/// Replace logical markers (`__C__col`, `__GRAIN__:Some(Month):expr`) with
/// dialect-quoting and dialect time functions.
fn materialize(
    expr: &str,
    quote: &dyn Fn(&str) -> String,
    grain_fn: &dyn Fn(&str, TimeGrain) -> String,
) -> String {
    if let Some(rest) = expr.strip_prefix("__GRAIN__:") {
        let (grain_s, col) = rest.split_once(':').unwrap_or((rest, ""));
        let grain = match grain_s.trim_start_matches("Some(").trim_end_matches(')') {
            "Day" => TimeGrain::Day,
            "Week" => TimeGrain::Week,
            "Quarter" => TimeGrain::Quarter,
            "Year" => TimeGrain::Year,
            _ => TimeGrain::Month,
        };
        return grain_fn(&materialize(col, quote, grain_fn), grain);
    }
    if !expr.contains("__C__") {
        return expr.to_string();
    }
    let mut out = String::new();
    let mut rest = expr;
    while let Some(i) = rest.find("__C__") {
        let (head, tail) = rest.split_at(i);
        out.push_str(head);
        rest = &tail[5..];
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let (id, tail2) = rest.split_at(end);
        out.push_str(&quote(id));
        rest = tail2;
    }
    out.push_str(rest);
    out
}

/// Per-dialect identifier quoting.
pub fn quote_fn(d: Dialect) -> impl Fn(&str) -> String {
    move |s: &str| match d {
        Dialect::Mysql => format!("`{}`", s.replace('`', "``")),
        Dialect::Pg | Dialect::Sqlite | Dialect::DuckDb => {
            format!("\"{}\"", s.replace('"', "\"\""))
        }
    }
}

/// Per-dialect date bucketing.
fn grain_expr_fn(d: Dialect) -> impl Fn(&str, TimeGrain) -> String {
    move |col: &str, g: TimeGrain| {
        match d {
        Dialect::Pg | Dialect::DuckDb => format!("DATE_TRUNC('{}', {col})", grain_name(g)),
        Dialect::Sqlite => match g {
            TimeGrain::Day => format!("STRFTIME('%Y-%m-%d', {col})"),
            TimeGrain::Week => format!("STRFTIME('%Y-%W', {col})"),
            TimeGrain::Month => format!("STRFTIME('%Y-%m', {col})"),
            TimeGrain::Quarter => format!(
                "STRFTIME('%Y', {col}) || '-Q' || ((CAST(STRFTIME('%m', {col}) AS INTEGER) + 2) / 3)"
            ),
            TimeGrain::Year => format!("STRFTIME('%Y', {col})"),
        },
        Dialect::Mysql => match g {
            TimeGrain::Day => format!("DATE({col})"),
            TimeGrain::Week => format!("DATE_SUB(DATE({col}), INTERVAL WEEKDAY({col}) DAY)"),
            TimeGrain::Month => format!("DATE_FORMAT({col}, '%Y-%m-01')"),
            TimeGrain::Quarter => format!("MAKEDATE(YEAR({col}), QUARTER({col}) * 3 - 2, 1)"),
            TimeGrain::Year => format!("DATE_FORMAT({col}, '%Y-01-01')"),
        },
    }
    }
}

fn grain_name(g: TimeGrain) -> &'static str {
    match g {
        TimeGrain::Day => "day",
        TimeGrain::Week => "week",
        TimeGrain::Month => "month",
        TimeGrain::Quarter => "quarter",
        TimeGrain::Year => "year",
    }
}

/// EXPLAIN prefix per dialect.
pub fn explain_prefix(d: Dialect) -> &'static str {
    match d {
        Dialect::Pg => "EXPLAIN ",
        Dialect::Mysql => "EXPLAIN ",
        Dialect::Sqlite => "EXPLAIN QUERY PLAN ",
        Dialect::DuckDb => "EXPLAIN ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::validate::validate;
    use crate::fixtures::shop_graph;
    use querora_contracts::*;

    fn planned() -> ResolvedQuery {
        let graph = shop_graph();
        let query = AnalyticalQuery {
            source: SourceId::new("shop"),
            measures: vec![MeasureRef {
                metric_id: "revenue".into(),
                alias: None,
            }],
            dimensions: vec![DimensionRef {
                dimension_id: "order_date".into(),
                grain: Some(TimeGrain::Month),
                alias: None,
            }],
            filters: vec![Filter {
                dimension_id: "order_status".into(),
                op: FilterOp::Eq,
                value: Some(FilterValue::Str("paid".into())),
            }],
            time: Some(TimeSpec {
                dimension_id: "order_date".into(),
                range: TimeRange::Between {
                    start: "2026-01-01".into(),
                    end: "2026-06-30".into(),
                },
                compare: None,
            }),
            order: vec![OrderSpec {
                key: "order_date".into(),
                direction: OrderDirection::Asc,
            }],
            limit: 100,
        };
        super::super::plan::plan(validate(&query, &graph).unwrap(), &graph).unwrap()
    }

    #[test]
    fn sqlite_render_shape() {
        let plan = render(&planned(), Dialect::Sqlite).unwrap();
        assert!(plan.sql.contains("STRFTIME('%Y-%m'"), "{}", plan.sql);
        assert!(plan.sql.contains("LIMIT 100"));
        assert_eq!(plan.params.len(), 3); // paid + start + end
    }

    #[test]
    fn pg_uses_dollar_placeholders() {
        let plan = render(&planned(), Dialect::Pg).unwrap();
        assert!(plan.sql.contains("DATE_TRUNC('month'"), "{}", plan.sql);
        assert!(
            plan.sql.contains("$1") && plan.sql.contains("$3"),
            "{}",
            plan.sql
        );
        assert!(!plan.sql.contains('?'));
    }

    #[test]
    fn mysql_uses_backticks() {
        let plan = render(&planned(), Dialect::Mysql).unwrap();
        assert!(plan.sql.contains("`orders`"), "{}", plan.sql);
        assert!(plan.sql.contains("DATE_FORMAT"));
    }

    #[test]
    fn duckdb_renders() {
        let plan = render(&planned(), Dialect::DuckDb).unwrap();
        assert!(plan.sql.contains("DATE_TRUNC('month'"));
        assert!(plan.sql.contains("\"orders\""));
    }

    #[test]
    fn every_render_parses_as_single_select() {
        for d in [
            Dialect::Pg,
            Dialect::Mysql,
            Dialect::Sqlite,
            Dialect::DuckDb,
        ] {
            let plan = render(&planned(), d).unwrap();
            crate::connectors::guard::assert_single_select(&plan.sql)
                .unwrap_or_else(|e| panic!("{d:?} render must parse: {e}\n{}", plan.sql));
        }
    }
}
