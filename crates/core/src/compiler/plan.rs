// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Planner: pick the base entity, walk the relationship graph (BFS, cycle
//! guard), and produce a single fully-resolved query shape the renderer
//! turns into dialect SQL. Ambiguous paths are REJECTED — the agent must
//! disambiguate (risk table: join fan-out duplicates).

use super::validate::{ValidatedMeasure, ValidatedQuery};
use chrono::{Datelike, Duration, Months, NaiveDate, Utc};
use querora_contracts::{ErrorCode, SemanticGraph, TimeRange, TimeUnit, ToolError};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// One SELECT list item (logical SQL + alias).
#[derive(Debug, Clone)]
pub struct SelectItem {
    /// Logical SQL expression (unquoted table refs as `table.column`).
    pub expr: String,
    /// Output alias.
    pub alias: String,
}

/// A parameterized WHERE condition.
#[derive(Debug, Clone)]
pub struct SqlCondition {
    /// Logical SQL (contains `?` placeholders in order).
    pub sql: String,
    /// Bound values (parallel to placeholders).
    pub params: Vec<serde_json::Value>,
}

/// One join step from the base entity outward.
#[derive(Debug, Clone)]
pub struct JoinStep {
    /// Table joined in.
    pub table: String,
    /// Unique alias.
    pub alias: String,
    /// Left side (`alias.col`) of the ON condition.
    pub on_left: String,
    /// Right side.
    pub on_right: String,
}

/// A fully planned measure.
#[derive(Debug, Clone)]
pub struct ResolvedMeasure {
    /// Output alias.
    pub alias: String,
    /// Logical SQL aggregate.
    pub sql: String,
}

/// The planned query: dialect-independent, placeholder-parameterized.
#[derive(Debug, Clone)]
pub struct ResolvedQuery {
    /// Base table (or CTE alias for virtual entities).
    pub base_table: String,
    /// Base alias.
    pub base_alias: String,
    /// Virtual-entity CTEs (alias, validated SELECT) rendered as WITH.
    pub ctes: Vec<(String, String)>,
    /// Joins in order.
    pub joins: Vec<JoinStep>,
    /// SELECT items.
    pub select: Vec<SelectItem>,
    /// WHERE conditions.
    pub conditions: Vec<SqlCondition>,
    /// GROUP BY aliases.
    pub group_by: Vec<String>,
    /// ORDER BY (alias, direction).
    pub order: Vec<(String, String)>,
    /// LIMIT.
    pub limit: u32,
    /// Resolved absolute time bounds (compile-time; trust panel).
    pub resolved_time_bounds: Option<(String, String)>,
    /// Entity-qualified column refs for every used dimension.
    pub dim_columns: BTreeMap<String, String>, // dim id -> "alias.col"
}

/// The compiler output. `sql` is final dialect SQL; the connector guard
/// re-verifies it is a single SELECT before execution.
#[derive(Debug, Clone)]
pub struct ExecutionPlan {
    /// Final SQL.
    pub sql: String,
    /// Bind parameters in placeholder order.
    pub params: Vec<serde_json::Value>,
    /// Statement timeout.
    pub timeout_secs: u32,
    /// Row cap.
    pub row_cap: u32,
    /// Semantic graph version pinned at compile time.
    pub semantic_version: String,
    /// Absolute time bounds when the query had a relative range.
    pub resolved_time_bounds: Option<(String, String)>,
    /// EXPLAIN-only plan (dry-run): must never be executed.
    pub explain_only: bool,
}

/// Plan the validated query.
pub fn plan(v: ValidatedQuery, graph: &SemanticGraph) -> Result<ResolvedQuery, ToolError> {
    // ---- base entity: prefer the entity of the first measure ----
    let base_entity = v
        .measures
        .first()
        .map(|m| m.metric.entity_id.clone())
        .or_else(|| v.dimensions.first().map(|d| d.dim.entity_id.clone()))
        .ok_or_else(|| ToolError::new(ErrorCode::InvalidIr, "nothing to query"))?;

    // ---- semi-join classification ----
    // Entities reachable ONLY for filtering via Semi relationships are not
    // LEFT-JOINed (no fan-out); their filters render as IN (SELECT …).
    let semi_entities: BTreeMap<String, &querora_contracts::semantic::Relationship> = graph
        .relationships
        .iter()
        .filter(|r| r.join_kind == querora_contracts::semantic::JoinKind::Semi)
        .map(|r| (r.to_entity.clone(), r))
        .collect();

    // ---- join paths for every other entity ----
    let needed: BTreeSet<String> = v.entities.clone();
    let mut joins: Vec<JoinStep> = Vec::new();
    let mut alias_of: BTreeMap<String, String> = BTreeMap::new();
    let base_alias = base_alias_for(&base_entity);
    alias_of.insert(base_entity.clone(), base_alias.clone());
    for entity in needed.iter() {
        if *entity == base_entity || alias_of.contains_key(entity) {
            continue;
        }
        // semi-related filter entities never JOIN (they subquery instead)
        if semi_entities.contains_key(entity) {
            continue;
        }
        let path = shortest_path(graph, &base_entity, entity)?;
        // walk path from base outward, materializing joins
        let mut cur = base_entity.clone();
        let mut cur_alias = base_alias.clone();
        for (next_entity, rel) in path {
            let next_table = graph
                .entities
                .get(&next_entity)
                .map(|e| e.table.clone())
                .ok_or_else(|| {
                    ToolError::new(ErrorCode::Internal, "broken graph: missing entity")
                })?;
            let next_alias = format!("t{}", joins.len() + 1);
            // relationship is from(fact)→to(dim): ON to.pk = from.fk
            // but BFS path direction can be either way — normalize:
            let (left, right) = if rel.from_entity == cur {
                (
                    format!("{next_alias}.{}", quote_col(&rel.to_column)),
                    format!("{cur_alias}.{}", quote_col(&rel.from_column)),
                )
            } else {
                (
                    format!("{next_alias}.{}", quote_col(&rel.from_column)),
                    format!("{cur_alias}.{}", quote_col(&rel.to_column)),
                )
            };
            joins.push(JoinStep {
                table: next_table,
                alias: next_alias.clone(),
                on_left: left,
                on_right: right,
            });
            alias_of.insert(next_entity.clone(), next_alias.clone());
            cur = next_entity;
            cur_alias = next_alias;
        }
    }

    let col_ref = |entity_id: &str, column: &str| -> String {
        let alias = alias_of
            .get(entity_id)
            .cloned()
            .unwrap_or_else(|| base_alias_for(entity_id));
        format!("{alias}.{}", quote_col(column))
    };

    // ---- select list ----
    let mut select = Vec::new();
    let mut dim_columns = BTreeMap::new();
    let mut group_by = Vec::new();
    for d in &v.dimensions {
        let col = col_ref(&d.dim.entity_id, &d.dim.column);
        dim_columns.insert(d.dim.id.clone(), col.clone());
        let expr = match d.dref.grain {
            Some(grain) => format!("__GRAIN__:{grain:?}:{col}"), // renderer maps per dialect
            None => col,
        };
        select.push(SelectItem {
            expr,
            alias: d.alias.clone(),
        });
        group_by.push(d.alias.clone());
    }
    for m in &v.measures {
        let sql = measure_sql(m, graph, &col_ref)?;
        select.push(SelectItem {
            expr: sql,
            alias: m.alias.clone(),
        });
    }

    // ---- where ----
    let mut conditions = Vec::new();
    let mut params: Vec<serde_json::Value> = Vec::new();
    for (dim, f) in &v.filters {
        // semi-join filter: col IN (SELECT to_column FROM <semi src> WHERE dim-cond)
        if let Some(rel) = semi_entities.get(&dim.entity_id).copied() {
            let semi_src = {
                let e = graph.entities.get(&dim.entity_id).ok_or_else(|| {
                    ToolError::new(ErrorCode::Internal, "broken graph: semi entity missing")
                })?;
                match &e.definition_sql {
                    Some(_) => format!("__CTE__{}", dim.entity_id),
                    None => e.table.clone(),
                }
            };
            let left_col = if rel.from_entity == base_entity {
                format!("{base_alias}.{}", quote_col(&rel.from_column))
            } else {
                col_ref(&rel.from_entity, &rel.from_column)
            };
            let inner_col = quote_col(&dim.column);
            let (op_sql, inner_params) = semi_inner_condition(f, &inner_col);
            conditions.push(SqlCondition {
                sql: format!(
                    "{left_col} IN (SELECT {} FROM {semi_src} WHERE {op_sql})",
                    quote_col(&rel.to_column)
                ),
                params: inner_params,
            });
            dim_columns.insert(dim.id.clone(), format!("__SEMI__{}", dim.id));
            continue;
        }
        let col = col_ref(&dim.entity_id, &dim.column);
        dim_columns.insert(dim.id.clone(), col.clone());
        let cond: String;
        let mut cparams = Vec::new();
        use querora_contracts::FilterOp as F;
        match f.op {
            F::IsNull => cond = format!("{col} IS NULL"),
            F::IsNotNull => cond = format!("{col} IS NOT NULL"),
            _ => {
                let val = f
                    .value
                    .clone()
                    .unwrap_or(querora_contracts::FilterValue::Null);
                let (op, n) = match f.op {
                    F::Eq => ("=", 1),
                    F::NotEq => ("<>", 1),
                    F::Gt => (">", 1),
                    F::Gte => (">=", 1),
                    F::Lt => ("<", 1),
                    F::Lte => ("<=", 1),
                    F::Like => ("LIKE", 1),
                    F::NotLike => ("NOT LIKE", 1),
                    F::In => ("IN", usize::MAX),
                    F::NotIn => ("NOT IN", usize::MAX),
                    _ => unreachable!(),
                };
                if let querora_contracts::FilterValue::List(items) = val {
                    if items.is_empty() {
                        return Err(ToolError::new(
                            ErrorCode::InvalidIr,
                            format!("filter on `{}` with empty list", f.dimension_id),
                        ));
                    }
                    let holes: Vec<&str> = items.iter().map(|_| "?").collect();
                    cond = format!("{col} {op} ({})", holes.join(", "));
                    for item in items {
                        cparams.push(filter_value(&item));
                    }
                } else if n == 1 {
                    cond = format!("{col} {op} ?");
                    cparams.push(filter_value(&val));
                } else {
                    return Err(ToolError::new(
                        ErrorCode::InvalidIr,
                        format!("operator {op:?} needs a list value on `{}`", f.dimension_id),
                    ));
                }
            }
        }
        params.extend(cparams.iter().cloned());
        conditions.push(SqlCondition {
            sql: cond,
            params: cparams,
        });
    }

    // ---- time range → concrete bounds + condition ----
    let mut resolved_time_bounds = None;
    if let Some((dim, range, _compare)) = &v.time {
        let col = col_ref(&dim.entity_id, &dim.column);
        dim_columns.insert(dim.id.clone(), col.clone());
        let (start, end) = match range {
            TimeRange::Last { count, unit } => {
                let today = Utc::now().date_naive();
                let end = today + Duration::days(1); // inclusive end, exclusive upper bound
                let start = sub_units(today, *count, *unit);
                (start, end)
            }
            TimeRange::Between { start, end } => (
                NaiveDate::parse_from_str(start, "%Y-%m-%d").map_err(|e| {
                    ToolError::new(
                        ErrorCode::InvalidIr,
                        format!("bad start date `{start}`: {e}"),
                    )
                })?,
                NaiveDate::parse_from_str(end, "%Y-%m-%d").map_err(|e| {
                    ToolError::new(ErrorCode::InvalidIr, format!("bad end date `{end}`: {e}"))
                })? + Duration::days(1),
            ),
        };
        resolved_time_bounds = Some((start.to_string(), end.to_string()));
        conditions.push(SqlCondition {
            sql: format!("{col} >= ? AND {col} < ?"),
            params: vec![
                serde_json::json!(start.to_string()),
                serde_json::json!(end.to_string()),
            ],
        });
    }

    let order = v
        .order
        .iter()
        .map(|(alias, _key, dir)| (alias.clone(), format!("{dir:?}").to_uppercase()))
        .collect();

    // virtual entities → CTEs; physical table for the rest
    let mut ctes: Vec<(String, String)> = Vec::new();
    let entity_ref = |id: &str| graph.entities.get(id);
    let cte_name = |id: &str| {
        format!(
            "v_{}",
            id.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .take(40)
                .collect::<String>()
        )
    };
    let mut ensure_cte = |entity_id: &str| -> Option<String> {
        let e = entity_ref(entity_id)?;
        if let Some(def) = &e.definition_sql {
            let name = cte_name(entity_id);
            if !ctes.iter().any(|(n, _)| n == &name) {
                ctes.push((name.clone(), def.clone()));
            }
            Some(name)
        } else {
            None
        }
    };
    let base_table = match ensure_cte(&base_entity) {
        Some(cte) => cte,
        None => graph
            .entities
            .get(&base_entity)
            .map(|e| e.table.clone())
            .unwrap_or_default(),
    };
    // join tables: virtual entities join their CTE
    for j in joins.iter_mut() {
        if let Some(eid) = graph
            .entities
            .iter()
            .find(|(_, e)| e.table == j.table)
            .map(|(k, _)| k.clone())
        {
            if let Some(cte) = ensure_cte(&eid) {
                j.table = cte;
            }
        }
    }

    // register CTEs for semi-related virtual entities used in filters
    for cond in conditions.iter_mut() {
        if let Some(marker) = cond.sql.find("__CTE__").and_then(|i| {
            cond.sql[i + 7..]
                .find(' ')
                .map(|e| cond.sql[i + 7..i + 7 + e].to_string())
        }) {
            let entity_id = marker;
            if let Some(e) = graph.entities.get(&entity_id) {
                if let Some(def) = &e.definition_sql {
                    let name = format!(
                        "v_{}",
                        entity_id
                            .chars()
                            .filter(|c| c.is_alphanumeric() || *c == '_')
                            .take(40)
                            .collect::<String>()
                    );
                    if !ctes.iter().any(|(n, _)| n == &name) {
                        ctes.push((name.clone(), def.clone()));
                    }
                    cond.sql = cond.sql.replace(&format!("__CTE__{entity_id}"), &name);
                }
            }
        }
    }

    Ok(ResolvedQuery {
        base_table,
        base_alias,
        ctes,
        joins,
        select,
        conditions,
        group_by,
        order,
        limit: v.limit,
        resolved_time_bounds,
        dim_columns,
    })
}

fn base_alias_for(entity: &str) -> String {
    let slug: String = entity
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(12)
        .collect();
    format!("q_{slug}")
}

fn quote_col(c: &str) -> String {
    format!("__C__{c}") // marker; renderer replaces with dialect quoting
}

pub(crate) fn filter_value_public(v: &querora_contracts::FilterValue) -> serde_json::Value {
    filter_value(v)
}

fn filter_value(v: &querora_contracts::FilterValue) -> serde_json::Value {
    match v {
        querora_contracts::FilterValue::Null => serde_json::Value::Null,
        querora_contracts::FilterValue::Bool(b) => serde_json::Value::from(*b),
        querora_contracts::FilterValue::Number(n) => serde_json::json!(n),
        querora_contracts::FilterValue::Str(s) => serde_json::Value::from(s.clone()),
        querora_contracts::FilterValue::List(l) => {
            serde_json::Value::Array(l.iter().map(filter_value).collect())
        }
    }
}

fn sub_units(today: NaiveDate, count: u32, unit: TimeUnit) -> NaiveDate {
    match unit {
        TimeUnit::Day => today - Duration::days(count as i64),
        TimeUnit::Week => today - Duration::weeks(count as i64),
        TimeUnit::Month => today
            .checked_sub_months(Months::new(count))
            .unwrap_or(today),
        TimeUnit::Quarter => today
            .checked_sub_months(Months::new(count.saturating_mul(3)))
            .unwrap_or(today),
        TimeUnit::Year => {
            let y = today.year() - count as i32;
            NaiveDate::from_ymd_opt(y, today.month(), today.day()).unwrap_or(today)
        }
    }
}

/// Measure SQL with ratio metrics resolved recursively (same entity).
fn measure_sql(
    m: &ValidatedMeasure,
    graph: &SemanticGraph,
    col_ref: &dyn Fn(&str, &str) -> String,
) -> Result<String, ToolError> {
    use querora_contracts::semantic::AggOp as A;
    let expr = &m.metric.expr;
    if let Some(comb) = &expr.combination {
        return render_combination(comb, &m.metric.entity_id, graph, col_ref);
    }
    let agg = |op: &str, col: &Option<String>| -> String {
        match col {
            Some(c) => format!("{op}({})", col_ref(&m.metric.entity_id, c)),
            None => format!("{op}(*)"),
        }
    };
    Ok(match &expr.op {
        A::Sum => agg("SUM", &expr.column),
        A::Avg => agg("AVG", &expr.column),
        A::Min => agg("MIN", &expr.column),
        A::Max => agg("MAX", &expr.column),
        A::Count => agg("COUNT", &expr.column),
        A::CountDistinct => format!(
            "COUNT(DISTINCT {})",
            agg("IDENT", &expr.column)
                .replace("IDENT(", "")
                .trim_end_matches(')')
        ),
        A::Ratio {
            numerator,
            denominator,
        } => {
            let num = graph
                .metrics
                .get(numerator)
                .ok_or_else(|| ToolError::new(ErrorCode::Internal, "broken ratio"))?;
            let den = graph
                .metrics
                .get(denominator)
                .ok_or_else(|| ToolError::new(ErrorCode::Internal, "broken ratio"))?;
            let num_sql = {
                let sub = ValidatedMeasure {
                    mref: querora_contracts::MeasureRef {
                        metric_id: String::new(),
                        alias: None,
                    },
                    metric: num.clone(),
                    alias: String::new(),
                };
                measure_sql(&sub, graph, col_ref)?
            };
            let den_sql = {
                let sub = ValidatedMeasure {
                    mref: querora_contracts::MeasureRef {
                        metric_id: String::new(),
                        alias: None,
                    },
                    metric: den.clone(),
                    alias: String::new(),
                };
                measure_sql(&sub, graph, col_ref)?
            };
            format!("({num_sql}) / NULLIF({den_sql}, 0)")
        }
    })
}

/// BFS shortest join path base→target. Returns entity/rel steps.
fn shortest_path(
    graph: &SemanticGraph,
    base: &str,
    target: &str,
) -> Result<Vec<(String, querora_contracts::semantic::Relationship)>, ToolError> {
    #[derive(Clone)]
    struct Node {
        entity: String,
        path: Vec<(String, querora_contracts::semantic::Relationship)>,
    }
    let mut queue: VecDeque<Node> = VecDeque::new();
    queue.push_back(Node {
        entity: base.to_string(),
        path: vec![],
    });
    let mut seen: BTreeSet<String> = BTreeSet::from([base.to_string()]);
    let mut found: Option<Vec<(String, querora_contracts::semantic::Relationship)>> = None;
    let mut path_count = 0;
    while let Some(node) = queue.pop_front() {
        if node.entity == target {
            if found.is_some() {
                path_count += 1; // second distinct path of equal-or-longer length
                if path_count >= 1 {
                    return Err(ambiguous(base, target));
                }
            }
            found = Some(node.path);
            continue;
        }
        for rel in &graph.relationships {
            for (a, b) in [
                (&rel.from_entity, &rel.to_entity),
                (&rel.to_entity, &rel.from_entity),
            ] {
                if a == &node.entity && !seen.contains(b) {
                    let mut path = node.path.clone();
                    path.push((b.clone(), rel.clone()));
                    seen.insert(b.clone());
                    queue.push_back(Node {
                        entity: b.clone(),
                        path,
                    });
                }
            }
        }
    }
    found.ok_or_else(|| ToolError::new(
        ErrorCode::InvalidIr,
        format!("no join path from `{base}` to `{target}` — add a relationship in the semantic layer"),
    ))
}

fn ambiguous(base: &str, target: &str) -> ToolError {
    ToolError::new(
        ErrorCode::AmbiguousJoin,
        format!("multiple join paths `{base}` → `{target}`; disambiguate by querying a single entity or adding a metric scoped to one entity"),
    )
}

/// Recursively render a metric arithmetic combination. Div guards the
/// right side with NULLIF(x, 0). Leaves render as plain aggregations.
#[allow(clippy::too_many_arguments)]
fn render_combination(
    comb: &querora_contracts::semantic::Combination,
    entity_id: &str,
    graph: &SemanticGraph,
    col_ref: &dyn Fn(&str, &str) -> String,
) -> Result<String, ToolError> {
    use querora_contracts::semantic::{AggOp as A, ArithOp};
    let leaf = |e: &querora_contracts::semantic::MetricExpr| -> String {
        match &e.op {
            A::Sum => format!(
                "SUM({})",
                e.column
                    .as_deref()
                    .map(|c| col_ref(entity_id, c))
                    .unwrap_or_else(|| "*".into())
            ),
            A::Avg => format!(
                "AVG({})",
                e.column
                    .as_deref()
                    .map(|c| col_ref(entity_id, c))
                    .unwrap_or_else(|| "*".into())
            ),
            A::Min => format!(
                "MIN({})",
                e.column
                    .as_deref()
                    .map(|c| col_ref(entity_id, c))
                    .unwrap_or_else(|| "*".into())
            ),
            A::Max => format!(
                "MAX({})",
                e.column
                    .as_deref()
                    .map(|c| col_ref(entity_id, c))
                    .unwrap_or_else(|| "*".into())
            ),
            A::Count => format!(
                "COUNT({})",
                e.column
                    .as_deref()
                    .map(|c| col_ref(entity_id, c))
                    .unwrap_or_else(|| "*".into())
            ),
            A::CountDistinct => format!(
                "COUNT(DISTINCT {})",
                e.column
                    .as_deref()
                    .map(|c| col_ref(entity_id, c))
                    .unwrap_or_else(|| "*".into())
            ),
            _ => String::new(),
        }
    };
    let side = |e: &querora_contracts::semantic::MetricExpr| -> Result<String, ToolError> {
        if let Some(comb) = &e.combination {
            render_combination(comb, entity_id, graph, col_ref)
        } else if let A::Ratio {
            numerator,
            denominator,
        } = &e.op
        {
            // Ratio inside a combination: resolve sub-metrics (must be plain
            // aggregations on the same entity — recursion handles nesting)
            let render_sub = |id: &str| -> Result<String, ToolError> {
                let sub_metric = graph.metrics.get(id).ok_or_else(|| {
                    ToolError::new(
                        ErrorCode::UnknownMetric,
                        format!("combination references unknown metric `{id}`"),
                    )
                })?;
                if sub_metric.entity_id != entity_id {
                    return Err(ToolError::new(
                        ErrorCode::InvalidIr,
                        format!(
                            "combination mixes entities: `{id}` lives on {}",
                            sub_metric.entity_id
                        ),
                    ));
                }
                let sub = ValidatedMeasure {
                    mref: querora_contracts::MeasureRef {
                        metric_id: id.to_string(),
                        alias: None,
                    },
                    metric: sub_metric.clone(),
                    alias: String::new(),
                };
                measure_sql(&sub, graph, col_ref)
            };
            let num = render_sub(numerator)?;
            let den = render_sub(denominator)?;
            Ok(format!("({num}) / NULLIF(({den}), 0)"))
        } else {
            Ok(leaf(e))
        }
    };
    let left = side(&comb.left)?;
    let right = side(&comb.right)?;
    Ok(match comb.op {
        ArithOp::Add => format!("(({left}) + ({right}))"),
        ArithOp::Sub => format!("(({left}) - ({right}))"),
        ArithOp::Mul => format!("(({left}) * ({right}))"),
        ArithOp::Div => format!("(({left}) / NULLIF(({right}), 0))"),
    })
}

/// Build the inner WHERE for a semi-join subquery (eq/in/like/like-not).
fn semi_inner_condition(
    f: &querora_contracts::Filter,
    inner_col: &str,
) -> (String, Vec<serde_json::Value>) {
    use querora_contracts::{FilterOp as F, FilterValue};
    let val = f.value.clone().unwrap_or(FilterValue::Null);
    match (&f.op, &val) {
        (F::Eq, v) => (
            format!("{inner_col} = ?"),
            vec![crate::compiler::plan::filter_value_public(v)],
        ),
        (F::NotEq, v) => (format!("{inner_col} <> ?"), vec![filter_value_public(v)]),
        (F::Like, v) => (format!("{inner_col} LIKE ?"), vec![filter_value_public(v)]),
        (F::NotLike, v) => (
            format!("{inner_col} NOT LIKE ?"),
            vec![filter_value_public(v)],
        ),
        (F::In, FilterValue::List(items)) => {
            let holes = items.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            (
                format!("{inner_col} IN ({holes})"),
                items.iter().map(filter_value_public).collect(),
            )
        }
        (F::NotIn, FilterValue::List(items)) => {
            let holes = items.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            (
                format!("{inner_col} NOT IN ({holes})"),
                items.iter().map(filter_value_public).collect(),
            )
        }
        _ => (format!("{inner_col} IS NOT NULL"), vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::super::validate::validate;
    use super::*;
    use crate::fixtures::shop_graph;
    use querora_contracts::*;

    #[test]
    fn joins_customer_for_country_dimension() {
        let graph = shop_graph();
        let query = AnalyticalQuery {
            source: SourceId::new("shop"),
            measures: vec![MeasureRef {
                metric_id: "revenue".into(),
                alias: None,
            }],
            dimensions: vec![DimensionRef {
                dimension_id: "customer_country".into(),
                grain: None,
                alias: None,
            }],
            filters: vec![],
            time: None,
            order: vec![],
            limit: 100,
        };
        let v = validate(&query, &graph).unwrap();
        let p = plan(v, &graph).unwrap();
        assert_eq!(p.joins.len(), 1);
        assert_eq!(p.joins[0].table, "customers");
        assert_eq!(p.group_by, vec!["d0"]);
    }

    #[test]
    fn relative_time_resolved_to_bounds() {
        let graph = shop_graph();
        let mut query = AnalyticalQuery {
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
            filters: vec![],
            time: Some(TimeSpec {
                dimension_id: "order_date".into(),
                range: TimeRange::Last {
                    count: 6,
                    unit: TimeUnit::Month,
                },
                compare: None,
            }),
            order: vec![],
            limit: 100,
        };
        query.limit = 50;
        let v = validate(&query, &graph).unwrap();
        let p = plan(v, &graph).unwrap();
        let (start, end) = p.resolved_time_bounds.expect("bounds");
        assert!(start.starts_with("202"), "start={start}");
        assert!(end.starts_with("202"), "end={end}");
        assert_eq!(p.limit, 50);
        // grain marker present for the renderer
        assert!(
            p.select[0].expr.starts_with("__GRAIN__:Month:"),
            "got {}",
            p.select[0].expr
        );
    }

    #[test]
    fn margin_combination_and_virtual_entity_cte() {
        use querora_contracts::semantic::*;
        use std::collections::BTreeMap;
        let graph = SemanticGraph {
            source: SourceId::new("magento"),
            version: "v1".into(),
            published: true,
            entities: BTreeMap::from([
                (
                    "order_items".to_string(),
                    Entity { id: "order_items".into(), label: "Order Items".into(), table: "sales_flat_order_item".into(), description: None, definition_sql: None },
                ),
                (
                    "item_cost".to_string(),
                    Entity {
                        id: "item_cost".into(),
                        label: "Item Cost".into(),
                        table: "item_cost".into(),
                        description: Some("per-item cost via EAV".into()),
                        definition_sql: Some(
                            "SELECT i.item_id, i.row_total, i.qty_ordered, c.value AS cost FROM sales_flat_order_item i JOIN product_cost_x c ON c.product_id = i.product_id".into(),
                        ),
                    },
                ),
            ]),
            metrics: BTreeMap::from([
                (
                    "item_revenue".to_string(),
                    Metric {
                        id: "item_revenue".into(),
                        label: "Item Revenue".into(),
                        entity_id: "item_cost".into(),
                        expr: MetricExpr { op: AggOp::Sum, column: Some("row_total".into()), human_formula: None, combination: None },
                        aliases: vec![],
                        description: None,
                    },
                ),
                (
                    "margin_pct".to_string(),
                    Metric {
                        id: "margin_pct".into(),
                        label: "Margin %".into(),
                        entity_id: "item_cost".into(),
                        expr: MetricExpr {
                            op: AggOp::Sum,
                            column: None,
                            human_formula: Some("(revenue - cost) / revenue".into()),
                            combination: Some(Combination {
                                left: Box::new(MetricExpr { op: AggOp::Sum, column: Some("row_total".into()), human_formula: None, combination: None }),
                                op: ArithOp::Sub,
                                right: Box::new(MetricExpr { op: AggOp::Sum, column: Some("cost".into()), human_formula: None, combination: None }),
                            }),
                        },
                        aliases: vec!["margin".into()],
                        description: None,
                    },
                ),
            ]),
            dimensions: BTreeMap::new(),
            relationships: vec![],
        value_index: Default::default(),
        };
        // margin = (sum(row_total) - sum(cost)) — validate + plan on virtual entity
        let q = AnalyticalQuery {
            source: SourceId::new("magento"),
            measures: vec![MeasureRef {
                metric_id: "margin_pct".into(),
                alias: None,
            }],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: 10,
        };
        let v = super::super::validate::validate(&q, &graph).unwrap();
        let p = plan(v, &graph).unwrap();
        assert!(
            p.ctes
                .iter()
                .any(|(n, d)| n.starts_with("v_item_cost") && d.contains("product_cost_x")),
            "cte: {:?}",
            p.ctes
        );
        assert_eq!(
            p.base_table,
            p.ctes[0].0.clone().split('.').next().unwrap().to_string()
        );
        let plan =
            super::super::render::render(&p, crate::connectors::types::Dialect::Mysql).unwrap();
        let sql = plan.sql.clone();
        assert!(sql.starts_with("WITH"), "sql: {sql}");
        assert!(sql.contains(") - (SUM("), "margin arithmetic: {sql}");
    }

    #[test]
    fn no_path_is_structured_error() {
        let mut graph = shop_graph();
        graph.relationships.clear();
        let query = AnalyticalQuery {
            source: SourceId::new("shop"),
            measures: vec![MeasureRef {
                metric_id: "revenue".into(),
                alias: None,
            }],
            dimensions: vec![DimensionRef {
                dimension_id: "customer_country".into(),
                grain: None,
                alias: None,
            }],
            filters: vec![],
            time: None,
            order: vec![],
            limit: 100,
        };
        let v = validate(&query, &graph).unwrap();
        let err = plan(v, &graph).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidIr);
        assert!(err.message.contains("no join path"));
    }
}
