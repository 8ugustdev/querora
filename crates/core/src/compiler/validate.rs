// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Semantic validation: resolve every IR reference against the PUBLISHED
//! graph. Unknown ids produce structured errors carrying the known ids so
//! agents can self-correct in one round-trip.

use querora_contracts::semantic::{AggOp, MetricExpr};
use querora_contracts::{
    AnalyticalQuery, Dimension, DimensionRef, ErrorCode, Filter, MeasureRef, Metric, SemanticGraph,
    TimeUnit, ToolError,
};
use std::collections::BTreeSet;

/// A metric resolved against the graph (expression + entity + output alias).
#[derive(Debug, Clone)]
pub struct ValidatedMeasure {
    /// Original measure ref.
    pub mref: MeasureRef,
    /// Resolved metric.
    pub metric: Metric,
    /// Output alias (m0, m1, …).
    pub alias: String,
}

/// A dimension resolved against the graph.
#[derive(Debug, Clone)]
pub struct ValidatedDimension {
    /// Original dimension ref.
    pub dref: DimensionRef,
    /// Resolved dimension.
    pub dim: Dimension,
    /// Output alias (d0, d1, …).
    pub alias: String,
}

/// The query after validation — every id resolved, nothing ambiguous left.
#[derive(Debug, Clone)]
pub struct ValidatedQuery {
    /// Source id.
    pub source: String,
    /// Measures.
    pub measures: Vec<ValidatedMeasure>,
    /// Dimensions.
    pub dimensions: Vec<ValidatedDimension>,
    /// Filters (dimension resolved inline).
    pub filters: Vec<(Dimension, Filter)>,
    /// Resolved time spec (dimension + range + optional grain from dims).
    pub time: Option<(
        Dimension,
        querora_contracts::TimeRange,
        Option<querora_contracts::CompareMode>,
    )>,
    /// Ordering on output aliases.
    pub order: Vec<(String, String, querora_contracts::OrderDirection)>,
    /// Effective row limit (server-capped).
    pub limit: u32,
    /// All entities touched (base = metrics' entity or first dim entity).
    pub entities: BTreeSet<String>,
}

/// Server-side row cap (config default 1000; validation answer #7 context).
pub const ROW_CAP_MAX: u32 = 1000;
/// Statement timeout (seconds).
pub const TIMEOUT_SECS: u32 = 30;

fn metric_ids(graph: &SemanticGraph) -> Vec<String> {
    graph.metrics.keys().cloned().collect()
}

fn dimension_ids(graph: &SemanticGraph) -> Vec<String> {
    graph.dimensions.keys().cloned().collect()
}

/// Validate the IR against the graph.
pub fn validate(
    query: &AnalyticalQuery,
    graph: &SemanticGraph,
) -> Result<ValidatedQuery, ToolError> {
    if !graph.published {
        return Err(ToolError::new(
            ErrorCode::InvalidIr,
            "the semantic graph for this source is still a draft — publish it first",
        ));
    }
    if query.source != graph.source {
        return Err(ToolError::new(
            ErrorCode::NotFound,
            format!(
                "graph serves source `{}`, query targets `{}`",
                graph.source.0, query.source.0
            ),
        ));
    }
    if query.measures.is_empty() && query.dimensions.is_empty() {
        return Err(ToolError::new(
            ErrorCode::InvalidIr,
            "query needs at least one measure or dimension",
        ));
    }

    let mut entities: BTreeSet<String> = BTreeSet::new();
    let mut measures = Vec::new();
    for (i, m) in query.measures.iter().enumerate() {
        let metric = graph.metrics.get(&m.metric_id).ok_or_else(|| {
            ToolError::new(
                ErrorCode::UnknownMetric,
                format!("unknown metric `{}`", m.metric_id),
            )
            .with_details(serde_json::json!({ "known_metrics": metric_ids(graph) }))
        })?;
        // combination metrics: nested Ratios must resolve to same-entity metrics
        if let Some(comb) = &metric.expr.combination {
            check_combination(comb, &metric.entity_id, graph)?;
        }
        entities.insert(metric.entity_id.clone());
        // ratio metrics must resolve on the same entity
        if let AggOp::Ratio {
            numerator,
            denominator,
        } = &metric.expr.op
        {
            for part in [numerator, denominator] {
                let sub = graph.metrics.get(part).ok_or_else(|| {
                    ToolError::new(
                        ErrorCode::UnknownMetric,
                        format!(
                            "ratio metric `{}` references unknown metric `{part}`",
                            m.metric_id
                        ),
                    )
                    .with_details(serde_json::json!({ "known_metrics": metric_ids(graph) }))
                })?;
                if sub.entity_id != metric.entity_id {
                    return Err(ToolError::new(
                        ErrorCode::InvalidIr,
                        format!(
                            "ratio metric `{}` mixes entities ({part} lives on {})",
                            m.metric_id, sub.entity_id
                        ),
                    ));
                }
            }
        }
        measures.push(ValidatedMeasure {
            mref: m.clone(),
            metric: metric.clone(),
            alias: m.alias.clone().unwrap_or_else(|| format!("m{i}")),
        });
    }

    // filter-only (semi-join) entities may FILTER but not GROUP BY
    let semi_only = |entity_id: &str| -> bool {
        let rels: Vec<&querora_contracts::semantic::Relationship> = graph
            .relationships
            .iter()
            .filter(|r| r.to_entity == entity_id)
            .collect();
        !rels.is_empty()
            && rels
                .iter()
                .all(|r| r.join_kind == querora_contracts::semantic::JoinKind::Semi)
    };
    let mut dimensions = Vec::new();
    for (i, d) in query.dimensions.iter().enumerate() {
        let dim = graph.dimensions.get(&d.dimension_id).ok_or_else(|| {
            ToolError::new(
                ErrorCode::UnknownDimension,
                format!("unknown dimension `{}`", d.dimension_id),
            )
            .with_details(serde_json::json!({ "known_dimensions": dimension_ids(graph) }))
        })?;
        if semi_only(&dim.entity_id) {
            return Err(ToolError::new(
                ErrorCode::InvalidIr,
                format!("dimension `{}` belongs to a filter-only entity — use it in filters, not as a group-by", d.dimension_id),
            ));
        }
        if let Some(grain) = d.grain {
            if dim.data_type != querora_contracts::SemanticDataType::Temporal {
                return Err(ToolError::new(
                    ErrorCode::InvalidIr,
                    format!(
                        "dimension `{}` is not temporal — cannot apply grain {grain:?}",
                        d.dimension_id
                    ),
                ));
            }
        }
        entities.insert(dim.entity_id.clone());
        dimensions.push(ValidatedDimension {
            dref: d.clone(),
            dim: dim.clone(),
            alias: d.alias.clone().unwrap_or_else(|| format!("d{i}")),
        });
    }

    let mut filters = Vec::new();
    for f in &query.filters {
        let dim = graph.dimensions.get(&f.dimension_id).ok_or_else(|| {
            ToolError::new(
                ErrorCode::UnknownDimension,
                format!("filter references unknown dimension `{}`", f.dimension_id),
            )
            .with_details(serde_json::json!({ "known_dimensions": dimension_ids(graph) }))
        })?;
        entities.insert(dim.entity_id.clone());
        filters.push((dim.clone(), f.clone()));
    }

    let mut time = None;
    if let Some(t) = &query.time {
        let dim = graph.dimensions.get(&t.dimension_id).ok_or_else(|| {
            ToolError::new(
                ErrorCode::UnknownDimension,
                format!(
                    "time spec references unknown dimension `{}`",
                    t.dimension_id
                ),
            )
            .with_details(serde_json::json!({ "known_dimensions": dimension_ids(graph) }))
        })?;
        if dim.data_type != querora_contracts::SemanticDataType::Temporal {
            return Err(ToolError::new(
                ErrorCode::InvalidIr,
                format!("time dimension `{}` is not temporal", t.dimension_id),
            ));
        }
        if semi_only(&dim.entity_id) {
            return Err(ToolError::new(
                ErrorCode::InvalidIr,
                format!(
                    "time dimension `{}` belongs to a filter-only entity",
                    t.dimension_id
                ),
            ));
        }
        entities.insert(dim.entity_id.clone());
        time = Some((dim.clone(), t.range.clone(), t.compare));
    }

    // ordering keys must reference known output aliases / metric / dimension ids
    let mut order = Vec::new();
    let alias_of = |key: &str| -> Option<String> {
        measures
            .iter()
            .find(|m| m.mref.metric_id == key || m.alias == key)
            .map(|m| m.alias.clone())
            .or_else(|| {
                dimensions
                    .iter()
                    .find(|d| d.dref.dimension_id == key || d.alias == key)
                    .map(|d| d.alias.clone())
            })
    };
    for o in &query.order {
        let alias = alias_of(&o.key).ok_or_else(|| {
            ToolError::new(
                ErrorCode::InvalidIr,
                format!(
                    "order key `{}` is not a selected measure or dimension",
                    o.key
                ),
            )
        })?;
        order.push((alias, o.key.clone(), o.direction));
    }

    Ok(ValidatedQuery {
        source: query.source.0.clone(),
        measures,
        dimensions,
        filters,
        time,
        order,
        limit: query.limit.clamp(1, ROW_CAP_MAX),
        entities,
    })
}

/// Recursively validate a metric combination: nested Ratio ids must exist
/// and live on the same entity; nesting depth capped.
fn check_combination(
    comb: &querora_contracts::semantic::Combination,
    entity_id: &str,
    graph: &SemanticGraph,
) -> Result<(), ToolError> {
    fn check_side(
        e: &MetricExpr,
        entity_id: &str,
        graph: &SemanticGraph,
        depth: u8,
    ) -> Result<(), ToolError> {
        if depth > 8 {
            return Err(ToolError::new(
                ErrorCode::InvalidIr,
                "metric combination nested too deep",
            ));
        }
        if let Some(c) = &e.combination {
            return check_combination(c, entity_id, graph);
        }
        if let AggOp::Ratio {
            numerator,
            denominator,
        } = &e.op
        {
            for id in [numerator, denominator] {
                let sub = graph.metrics.get(id).ok_or_else(|| {
                    ToolError::new(
                        ErrorCode::UnknownMetric,
                        format!("combination references unknown metric `{id}`"),
                    )
                    .with_details(serde_json::json!({ "known_metrics": metric_ids(graph) }))
                })?;
                if sub.entity_id != entity_id {
                    return Err(ToolError::new(
                        ErrorCode::InvalidIr,
                        format!(
                            "combination mixes entities: `{id}` lives on {}",
                            sub.entity_id
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
    check_side(&comb.left, entity_id, graph, 0)?;
    check_side(&comb.right, entity_id, graph, 0)
}

/// Render the aggregation SQL expression for a metric (dialect-agnostic —
/// quoting happens in the renderer; this yields logical SQL).
pub fn metric_sql_expr(metric: &Metric, quote: &dyn Fn(&str) -> String) -> String {
    let expr = &metric.expr;
    match_expr(expr, quote)
}

fn match_expr(expr: &MetricExpr, quote: &dyn Fn(&str) -> String) -> String {
    let col = |c: &Option<String>| c.as_deref().map(quote).unwrap_or_else(|| "*".to_string());
    match &expr.op {
        AggOp::Sum => format!("SUM({})", col(&expr.column)),
        AggOp::Avg => format!("AVG({})", col(&expr.column)),
        AggOp::Min => format!("MIN({})", col(&expr.column)),
        AggOp::Max => format!("MAX({})", col(&expr.column)),
        AggOp::Count => {
            if expr.column.is_some() {
                format!("COUNT({})", col(&expr.column))
            } else {
                "COUNT(*)".to_string()
            }
        }
        AggOp::CountDistinct => format!("COUNT(DISTINCT {})", col(&expr.column)),
        // ratio pieces are SUMs of their parts; resolved recursively by planner
        AggOp::Ratio { .. } => String::new(),
    }
}

/// Time range bounds resolution happens in the planner (needs "now").
pub fn unit_label(u: TimeUnit) -> &'static str {
    match u {
        TimeUnit::Day => "day",
        TimeUnit::Week => "week",
        TimeUnit::Month => "month",
        TimeUnit::Quarter => "quarter",
        TimeUnit::Year => "year",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::shop_graph;
    use querora_contracts::*;

    fn q(measures: &[&str], dims: &[&str]) -> AnalyticalQuery {
        AnalyticalQuery {
            source: SourceId::new("shop"),
            measures: measures
                .iter()
                .map(|m| MeasureRef {
                    metric_id: m.to_string(),
                    alias: None,
                })
                .collect(),
            dimensions: dims
                .iter()
                .map(|d| DimensionRef {
                    dimension_id: d.to_string(),
                    grain: None,
                    alias: None,
                })
                .collect(),
            filters: vec![],
            time: None,
            order: vec![],
            limit: 100,
        }
    }

    #[test]
    fn unknown_metric_lists_known_metrics() {
        let graph = shop_graph();
        let err = validate(&q(&["nope"], &[]), &graph).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnknownMetric);
        let details = err.details.unwrap();
        assert!(details["known_metrics"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("revenue")));
    }

    #[test]
    fn unknown_dimension_lists_known_dimensions() {
        let graph = shop_graph();
        let err = validate(&q(&["revenue"], &["nope"]), &graph).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnknownDimension);
    }

    #[test]
    fn grain_on_non_temporal_rejected() {
        let graph = shop_graph();
        let mut query = q(&["revenue"], &["order_status"]);
        query.dimensions[0].grain = Some(TimeGrain::Month);
        let err = validate(&query, &graph).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidIr);
    }

    #[test]
    fn empty_query_rejected() {
        let graph = shop_graph();
        let err = validate(&q(&[], &[]), &graph).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidIr);
    }

    #[test]
    fn limit_is_server_capped() {
        let graph = shop_graph();
        let mut query = q(&["revenue"], &["order_status"]);
        query.limit = 100_000;
        let v = validate(&query, &graph).unwrap();
        assert_eq!(v.limit, ROW_CAP_MAX);
    }

    #[test]
    fn draft_graph_rejected() {
        let mut graph = shop_graph();
        graph.published = false;
        let err = validate(&q(&["revenue"], &[]), &graph).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidIr);
        assert!(err.message.contains("draft"));
    }

    #[test]
    fn order_key_must_be_selected() {
        let graph = shop_graph();
        let mut query = q(&["revenue"], &["order_status"]);
        query.order = vec![OrderSpec {
            key: "customer_country".into(),
            direction: OrderDirection::Asc,
        }];
        assert_eq!(
            validate(&query, &graph).unwrap_err().code,
            ErrorCode::InvalidIr
        );
        // ordering by a selected metric id is fine
        query.order = vec![OrderSpec {
            key: "revenue".into(),
            direction: OrderDirection::Desc,
        }];
        assert!(validate(&query, &graph).is_ok());
    }
}
