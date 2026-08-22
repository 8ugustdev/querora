// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Built-in tools — the single tool surface wrapped by MCP (Phase 5) and
//! the pi sidecar. Agent-facing payloads are always truncated/safe.

use super::registry::{QueroraTool, ToolContext};
use async_trait::async_trait;
use querora_contracts::{ErrorCode, SourceId, ToolError};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

/// Params for `search_semantics`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchSemanticsParams {
    /// Natural-language search terms, e.g. "revenue by month".
    pub query: String,
    /// Restrict to one source (optional).
    #[serde(default)]
    pub source: Option<String>,
    /// Max items returned (default 10, cap 20).
    #[serde(default)]
    pub k: Option<u8>,
}

/// Params for `get_schema`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetSchemaParams {
    /// Source id.
    pub source: String,
    /// Optional table filter.
    #[serde(default)]
    pub table: Option<String>,
}

/// Params for `profile_column`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProfileColumnParams {
    /// Source id.
    pub source: String,
    /// Table name.
    pub table: String,
    /// Column name.
    pub column: String,
}

/// Params for `execute_query`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExecuteQueryParams {
    /// The analytical IR. NEVER SQL.
    pub ir: querora_contracts::AnalyticalQuery,
}

/// Params for `dry_run`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DryRunParams {
    /// The analytical IR to compile (not execute).
    pub ir: querora_contracts::AnalyticalQuery,
}

fn schema_for<T: JsonSchema>() -> serde_json::Value {
    let schema = schemars::schema_for!(T);
    serde_json::to_value(schema).unwrap_or(serde_json::json!({}))
}

/// `search_semantics` — retrieval over the semantic graph. Phase 2 ships a
/// normalized-substring stub over the fixture graph; Phase 7 swaps in FTS5.
pub struct SearchSemanticsTool;

#[async_trait]
impl QueroraTool for SearchSemanticsTool {
    fn name(&self) -> &'static str {
        "search_semantics"
    }

    fn description(&self) -> String {
        "Search the published semantic layer (metrics, dimensions, entities). \
         Use this FIRST to translate a business question into metric/dimension ids \
         before composing an analytical query."
            .into()
    }

    fn params_schema(&self) -> serde_json::Value {
        schema_for::<SearchSemanticsParams>()
    }

    async fn handle(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        let p: SearchSemanticsParams = serde_json::from_value(params)
            .map_err(|e| ToolError::new(ErrorCode::InvalidIr, e.to_string()))?;
        let graph = ctx.semantic().ok_or_else(|| {
            ToolError::new(ErrorCode::NotFound, "no semantic graph is being served yet")
        })?;
        if let Some(src) = &p.source {
            if *src != graph.source.0 {
                return Err(ToolError::new(
                    ErrorCode::NotFound,
                    format!("no semantic graph for source `{src}`"),
                )
                .with_details(serde_json::json!({ "available_source": graph.source.0 })));
            }
        }

        let k = p.k.unwrap_or(10).min(20) as usize;

        // FTS5 path (Phase 7): alias/prefix retrieval over the published graph
        if let Ok(hits) = crate::semantic::retrieval::search(&ctx.store, &p.query, k).await {
            if !hits.is_empty() {
                let items: Vec<serde_json::Value> = hits
                    .iter()
                    .map(|h| {
                        let body = match h.kind.as_str() {
                            "value" => {
                                // value hit: resolve owning dimension + the matched value
                                graph.dimensions.get(&h.id).map(|d| {
                                    // plural-stripped containment so "lamps" matches
                                    // "Bordlamper"
                                    let toks: Vec<String> = normalize_query(&p.query)
                                        .split_whitespace()
                                        .flat_map(|t| {
                                            let mut t = t.to_string();
                                            let mut out = vec![t.clone()];
                                            if t.ends_with('s') {
                                                t.pop();
                                                out.push(t);
                                            }
                                            out
                                        })
                                        .collect();
                                    let example = std::iter::once(h.label.clone())
                                        .chain(graph.value_index.get(&h.id).cloned().unwrap_or_default())
                                        .find(|v| {
                                            let lv = v.to_lowercase();
                                            toks.iter().any(|t| lv.contains(t.as_str()))
                                        });
                                    serde_json::json!({
                                        "kind": "dimension", "id": d.id, "label": d.label,
                                        "entity_id": d.entity_id, "data_type": d.data_type,
                                        "description": d.description, "aliases": d.aliases,
                                        "matched_value": example,
                                        "hint": format!("filter `{}` with this value", d.id),
                                    })
                                })
                            }
                            "metric" => graph.metrics.get(&h.id).map(|m| serde_json::json!({
                                "kind": "metric", "id": m.id, "label": m.label, "entity_id": m.entity_id,
                                "description": m.description, "aliases": m.aliases,
                            })),
                            "dimension" => graph.dimensions.get(&h.id).map(|d| serde_json::json!({
                                "kind": "dimension", "id": d.id, "label": d.label, "entity_id": d.entity_id,
                                "data_type": d.data_type, "description": d.description, "aliases": d.aliases,
                            })),
                            _ => graph.entities.get(&h.id).map(|e| serde_json::json!({
                                "kind": "entity", "id": e.id, "label": e.label, "table": e.table,
                                "description": e.description,
                            })),
                        };
                        body.unwrap_or(serde_json::json!({ "kind": h.kind, "id": h.id, "label": h.label }))
                    })
                    .collect();
                return Ok(serde_json::json!({
                    "source": graph.source.0,
                    "semantic_version": graph.version,
                    "query": p.query,
                    "items": items,
                    "note": "compose AnalyticalQuery IR with these ids; emit IR, never SQL",
                }));
            }
        }

        let terms: Vec<String> = p
            .query
            .to_lowercase()
            .split_whitespace()
            .map(normalize)
            .filter(|t| !t.is_empty() && !STOPWORDS.contains(&t.as_str()))
            .collect();

        let mut hits: Vec<(i32, serde_json::Value)> = Vec::new();
        // score = number of query terms matched in name/aliases/labels
        for (id, m) in &graph.metrics {
            let score = terms
                .iter()
                .filter(|t| matches_text(t, &[m.label.as_str(), id.as_str()], &m.aliases))
                .count() as i32
                + if m
                    .description
                    .as_deref()
                    .map(|d| terms.iter().any(|t| d.to_lowercase().contains(t.as_str())))
                    .unwrap_or(false)
                {
                    1
                } else {
                    0
                };
            if score > 0 {
                hits.push((
                    score,
                    serde_json::json!({
                        "kind": "metric", "id": id, "label": m.label, "entity_id": m.entity_id,
                        "description": m.description, "aliases": m.aliases,
                    }),
                ));
            }
        }
        for (id, d) in &graph.dimensions {
            let score = terms
                .iter()
                .filter(|t| matches_text(t, &[d.label.as_str(), id.as_str()], &d.aliases))
                .count() as i32;
            if score > 0 {
                hits.push((
                    score,
                    serde_json::json!({
                        "kind": "dimension", "id": id, "label": d.label, "entity_id": d.entity_id,
                        "data_type": d.data_type, "description": d.description, "aliases": d.aliases,
                    }),
                ));
            }
        }
        for (id, e) in &graph.entities {
            let score = terms
                .iter()
                .filter(|t| matches_text(t, &[e.label.as_str(), id.as_str()], &[]))
                .count() as i32;
            if score > 0 {
                hits.push((
                    score,
                    serde_json::json!({
                        "kind": "entity", "id": id, "label": e.label, "table": e.table,
                        "description": e.description,
                    }),
                ));
            }
        }

        hits.sort_by_key(|(score, item)| (std::cmp::Reverse(*score), item.to_string()));
        let items: Vec<_> = hits.into_iter().take(k).map(|(_, v)| v).collect();
        Ok(serde_json::json!({
            "source": graph.source.0,
            "semantic_version": graph.version,
            "query": p.query,
            "items": items,
            "note": "compose AnalyticalQuery IR with these ids; emit IR, never SQL",
        }))
    }
}

/// `get_schema` — physical catalog introspection via the connector registry.
/// The catalog is also cached in the app db (drift reports consume it).
pub struct GetSchemaTool;

#[async_trait]
impl QueroraTool for GetSchemaTool {
    fn name(&self) -> &'static str {
        "get_schema"
    }

    fn description(&self) -> String {
        "List tables and columns of a connected source (physical schema). Prefer \
         search_semantics; use this only when the semantic layer lacks what you need."
            .into()
    }

    fn params_schema(&self) -> serde_json::Value {
        schema_for::<GetSchemaParams>()
    }

    async fn handle(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        let p: GetSchemaParams = serde_json::from_value(params)
            .map_err(|e| ToolError::new(ErrorCode::InvalidIr, e.to_string()))?;
        let id = SourceId::new(p.source.clone());
        let ds = ctx.sources.get(&id, &ctx.store, ctx.creds.as_ref()).await?;
        let catalog = ds.catalog().await?;
        ctx.store
            .set_catalog(&id, &catalog)
            .await
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
        let tables: Vec<_> = catalog
            .tables
            .iter()
            .filter(|t| p.table.as_deref().map(|f| t.name == f).unwrap_or(true))
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "is_view": t.is_view,
                    "columns": t.columns.iter().map(|c| serde_json::json!({
                        "name": c.name, "data_type": c.data_type,
                        "nullable": c.nullable, "primary_key": c.primary_key,
                    })).collect::<Vec<_>>(),
                    "foreign_keys": t.foreign_keys.iter().map(|f| serde_json::json!({
                        "column": f.column, "references": format!("{}.{}", f.ref_table, f.ref_column),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(serde_json::json!({
            "source": p.source,
            "dialect": format!("{:?}", ds.dialect()).to_lowercase(),
            "tables": tables,
        }))
    }
}

/// `profile_column` — sampled column statistics via the connector.
pub struct ProfileColumnTool;

#[async_trait]
impl QueroraTool for ProfileColumnTool {
    fn name(&self) -> &'static str {
        "profile_column"
    }

    fn description(&self) -> String {
        "Profile a column (distinct count, null %, min/max, top values). Useful \
         to validate filter values before querying."
            .into()
    }

    fn params_schema(&self) -> serde_json::Value {
        schema_for::<ProfileColumnParams>()
    }

    async fn handle(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        let p: ProfileColumnParams = serde_json::from_value(params)
            .map_err(|e| ToolError::new(ErrorCode::InvalidIr, e.to_string()))?;
        let id = SourceId::new(p.source.clone());
        let ds = ctx.sources.get(&id, &ctx.store, ctx.creds.as_ref()).await?;
        let profile = ds.profile(&p.table, &p.column, 10_000).await?;
        serde_json::to_value(profile)
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))
    }
}

/// Resolve the graph a query should compile against: the PUBLISHED graph
/// from the app store, falling back to the in-context served fixture graph
/// (dogfooding/tests) when it matches the source.
async fn graph_for(
    ctx: &ToolContext,
    source: &str,
) -> Result<(Arc<querora_contracts::SemanticGraph>, &'static str), ToolError> {
    if let Ok(Some(g)) = ctx.store.published_graph(&SourceId::new(source)).await {
        return Ok((Arc::new(g), "published"));
    }
    if let Some(g) = ctx.semantic() {
        if g.source.0 == source {
            return Ok((g, "fixture"));
        }
    }
    Err(ToolError::new(
        ErrorCode::NotFound,
        format!("no published semantic graph for source `{source}`"),
    )
    .with_details(serde_json::json!({ "hint": "publish a semantic model for this source first" })))
}

/// `execute_query` — IR in → truncated result out. Compiler + connector
/// end-to-end; full rows never enter agent context (head ≤ 50 + result_id).
pub struct ExecuteQueryTool;

#[async_trait]
impl QueroraTool for ExecuteQueryTool {
    fn name(&self) -> &'static str {
        "execute_query"
    }

    fn description(&self) -> String {
        "Execute a validated AnalyticalQuery (IR). Returns truncated rows (≤50) \
         + stats + result_id + the SQL Querora compiled. Never write SQL yourself."
            .into()
    }

    fn params_schema(&self) -> serde_json::Value {
        schema_for::<ExecuteQueryParams>()
    }

    async fn handle(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        let p: ExecuteQueryParams = serde_json::from_value(params)
            .map_err(|e| ToolError::new(ErrorCode::InvalidIr, e.to_string()))?;
        let result = execute_ir(ctx, &p.ir).await?;
        let agent: querora_contracts::AgentResult = (&result).into();
        ctx.results.put(result);
        serde_json::to_value(agent).map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))
    }
}

/// `dry_run` — compile the IR and return the ExecutionPlan without running.
pub struct DryRunTool;

#[async_trait]
impl QueroraTool for DryRunTool {
    fn name(&self) -> &'static str {
        "dry_run"
    }

    fn description(&self) -> String {
        "Compile an AnalyticalQuery and return the plan (SQL, params, resolved \
         time bounds) WITHOUT executing it. Use to validate IR before running."
            .into()
    }

    fn params_schema(&self) -> serde_json::Value {
        schema_for::<DryRunParams>()
    }

    async fn handle(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        let p: DryRunParams = serde_json::from_value(params)
            .map_err(|e| ToolError::new(ErrorCode::InvalidIr, e.to_string()))?;
        let id = p.ir.source.clone();
        let ds = ctx.sources.get(&id, &ctx.store, ctx.creds.as_ref()).await?;
        let (graph, origin) = graph_for(ctx, &p.ir.source.0).await?;
        let plan = crate::compiler::compile(&p.ir, &graph, ds.dialect())?;
        Ok(serde_json::json!({
            "sql": plan.sql,
            "params": plan.params,
            "row_cap": plan.row_cap,
            "timeout_secs": plan.timeout_secs,
            "resolved_time_bounds": plan.resolved_time_bounds,
            "semantic_version": graph.version,
            "semantic_origin": origin,
            "executed": false,
        }))
    }
}

/// Shared execution pipeline: validate → compile → connect → execute →
/// full `QueryResult` (app-side; callers decide what crosses the boundary).
pub async fn execute_ir(
    ctx: &ToolContext,
    ir: &querora_contracts::AnalyticalQuery,
) -> Result<querora_contracts::QueryResult, ToolError> {
    // period-over-period: compile+run the shifted twin, then stitch
    if ir.time.as_ref().and_then(|t| t.compare).is_some() {
        return execute_ir_compare(ctx, ir).await;
    }
    execute_ir_single(ctx, ir).await
}

/// Previous period of the same length (relative or absolute ranges).
fn previous_range(range: &querora_contracts::TimeRange) -> querora_contracts::TimeRange {
    use querora_contracts::TimeRange;
    match range {
        TimeRange::Last { count, unit } => {
            // current: [now − N units, now] → previous: [now − 2N, now − N]
            let doubled = count.saturating_mul(2);
            TimeRange::Last {
                count: doubled,
                unit: *unit,
            }
        }
        TimeRange::Between { start, end } => {
            let parse = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
            if let (Some(s), Some(e)) = (parse(start), parse(end)) {
                let len = (e - s).num_days().max(1);
                let new_end = s - chrono::Duration::days(1);
                let new_start = new_end - chrono::Duration::days(len);
                TimeRange::Between {
                    start: new_start.to_string(),
                    end: new_end.to_string(),
                }
            } else {
                range.clone()
            }
        }
    }
}

/// Run current + previous period and merge into one result with a
/// `period` column ("current" | "previous") for charting side-by-side.
async fn execute_ir_compare(
    ctx: &ToolContext,
    ir: &querora_contracts::AnalyticalQuery,
) -> Result<querora_contracts::QueryResult, ToolError> {
    let mut prev_ir = ir.clone();
    let time = prev_ir.time.as_mut().expect("checked by caller");
    let prev_range = previous_range(&time.range);
    let cur_range = time.range.clone();
    // PreviousPeriod with Last{2N} is wrong for stitching — recompute as
    // explicit Between windows so both halves are precise.
    if let (querora_contracts::TimeRange::Last { count, unit }, Some(dim)) =
        (&cur_range, ir.time.as_ref().map(|t| t.dimension_id.clone()))
    {
        let today = chrono::Utc::now().date_naive();
        let (cs, ce) = shift_last(today, *count, *unit, 0);
        let (ps, pe) = shift_last(today, *count, *unit, *count);
        let _ = dim;
        ir_time_set(&mut prev_ir, ps.to_string(), pe.to_string());
        // current twin as explicit Between too (stable vs 'now' drift)
        let mut cur_ir = ir.clone();
        ir_time_set(&mut cur_ir, cs.to_string(), ce.to_string());
        return stitch(ctx, ir, &cur_ir, &prev_ir).await;
    }
    time.range = prev_range;
    time.compare = None;
    stitch(ctx, ir, ir, &prev_ir).await
}

fn ir_time_set(ir: &mut querora_contracts::AnalyticalQuery, start: String, end: String) {
    if let Some(t) = ir.time.as_mut() {
        t.range = querora_contracts::TimeRange::Between { start, end };
        t.compare = None;
    }
}

/// (start, endExclusive) for a Last{count, unit} window offset by `back`
/// additional units.
fn shift_last(
    today: chrono::NaiveDate,
    count: u32,
    unit: querora_contracts::TimeUnit,
    back: u32,
) -> (chrono::NaiveDate, chrono::NaiveDate) {
    use chrono::{Datelike, Months};
    let end = match unit {
        querora_contracts::TimeUnit::Day => {
            today - chrono::Duration::days(back as i64) + chrono::Duration::days(1)
        }
        querora_contracts::TimeUnit::Week => {
            today - chrono::Duration::weeks(back as i64) + chrono::Duration::days(1)
        }
        querora_contracts::TimeUnit::Month => {
            today.checked_sub_months(Months::new(back)).unwrap_or(today)
        }
        querora_contracts::TimeUnit::Quarter => today
            .checked_sub_months(Months::new(back.saturating_mul(3)))
            .unwrap_or(today),
        querora_contracts::TimeUnit::Year => today
            .checked_sub_months(Months::new(back.saturating_mul(12)))
            .unwrap_or(today),
    };
    let start = match unit {
        querora_contracts::TimeUnit::Day => end - chrono::Duration::days(count as i64),
        querora_contracts::TimeUnit::Week => end - chrono::Duration::weeks(count as i64),
        querora_contracts::TimeUnit::Month => {
            end.checked_sub_months(Months::new(count)).unwrap_or(end)
        }
        querora_contracts::TimeUnit::Quarter => end
            .checked_sub_months(Months::new(count.saturating_mul(3)))
            .unwrap_or(end),
        querora_contracts::TimeUnit::Year => {
            let y = end.year() - count as i32;
            chrono::NaiveDate::from_ymd_opt(y, end.month(), end.day()).unwrap_or(end)
        }
    };
    (start, end)
}

async fn stitch(
    ctx: &ToolContext,
    _original: &querora_contracts::AnalyticalQuery,
    cur_ir: &querora_contracts::AnalyticalQuery,
    prev_ir: &querora_contracts::AnalyticalQuery,
) -> Result<querora_contracts::QueryResult, ToolError> {
    let cur = execute_ir_single(ctx, cur_ir).await?;
    let prev = execute_ir_single(ctx, prev_ir).await?;
    let mut columns = vec!["period".to_string()];
    columns.extend(cur.columns.iter().cloned());
    let mut column_types = vec![querora_contracts::ColumnMeta::String];
    column_types.extend(cur.column_types.iter().cloned());
    let mut rows: Vec<querora_contracts::Row> = Vec::new();
    for tag in [("previous", &prev), ("current", &cur)] {
        for r in &tag.1.rows {
            let mut m = std::collections::BTreeMap::new();
            m.insert(
                "period".to_string(),
                serde_json::Value::String(tag.0.to_string()),
            );
            for (c, v) in r {
                m.insert(c.clone(), v.clone());
            }
            rows.push(m);
        }
    }
    let row_count = rows.len() as u64;
    Ok(querora_contracts::QueryResult {
        result_id: uuid::Uuid::new_v4().to_string(),
        columns,
        column_types,
        rows,
        sql: format!("{}\n-- previous period:\n{}", cur.sql, prev.sql),
        params: [cur.params, prev.params].concat(),
        semantic_version: cur.semantic_version.clone(),
        stats: querora_contracts::ResultStats {
            row_count,
            duration_ms: cur.stats.duration_ms + prev.stats.duration_ms,
            row_cap: cur.stats.row_cap,
            timeout_secs: cur.stats.timeout_secs,
        },
    })
}

async fn execute_ir_single(
    ctx: &ToolContext,
    ir: &querora_contracts::AnalyticalQuery,
) -> Result<querora_contracts::QueryResult, ToolError> {
    let id = ir.source.clone();
    let ds = ctx.sources.get(&id, &ctx.store, ctx.creds.as_ref()).await?;
    let (graph, _origin) = graph_for(ctx, &ir.source.0).await?;
    let plan = crate::compiler::compile(ir, &graph, ds.dialect())?;
    let t0 = std::time::Instant::now();
    let raw = ds
        .execute(
            &plan.sql,
            &plan.params,
            crate::connectors::RowCap {
                limit: plan.row_cap,
                timeout_secs: plan.timeout_secs,
            },
        )
        .await?;
    let duration_ms = t0.elapsed().as_millis() as u64;
    let row_count = raw.rows.len() as u64;
    let rows: Vec<querora_contracts::Row> = raw
        .rows
        .iter()
        .map(|r| {
            raw.columns
                .iter()
                .zip(r.iter())
                .map(|(c, v)| (c.clone(), v.clone()))
                .collect::<std::collections::BTreeMap<String, serde_json::Value>>()
        })
        .collect();
    Ok(querora_contracts::QueryResult {
        result_id: uuid::Uuid::new_v4().to_string(),
        columns: raw.columns.clone(),
        column_types: raw.column_types.clone(),
        rows,
        sql: plan.sql.clone(),
        params: plan.params.clone(),
        semantic_version: graph.version.clone(),
        stats: querora_contracts::ResultStats {
            row_count,
            duration_ms,
            row_cap: plan.row_cap,
            timeout_secs: plan.timeout_secs,
        },
    })
}

fn normalize_query(q: &str) -> String {
    q.split_whitespace()
        .filter(|t| !STOPWORDS.contains(&t.to_lowercase().as_str()))
        .map(normalize)
        .find(|t| !t.is_empty())
        .unwrap_or_default()
}

/// Terms with no retrieval value.
const STOPWORDS: &[&str] = &[
    "by", "per", "the", "a", "an", "of", "for", "in", "on", "to", "and", "show", "me", "what",
    "which", "is", "are",
];

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

fn matches_text(term: &str, texts: &[&str], aliases: &[String]) -> bool {
    texts.iter().any(|t| normalize(t).contains(term))
        || aliases.iter().any(|a| {
            let a = normalize(a);
            a == *term || a.split(['_', '-']).any(|p| p == term)
        })
}

/// Register the default toolset on a registry.
pub fn register_defaults(registry: &super::registry::ToolRegistry) {
    registry.register(Arc::new(SearchSemanticsTool));
    registry.register(Arc::new(GetSchemaTool));
    registry.register(Arc::new(ProfileColumnTool));
    registry.register(Arc::new(ExecuteQueryTool));
    registry.register(Arc::new(DryRunTool));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::shop_graph;
    use crate::keyring::MemoryStore;
    use crate::storage::AppStore;
    use std::str::FromStr;
    use std::sync::Arc;

    async fn ctx() -> ToolContext {
        let store = AppStore::open_in_memory().await.unwrap();
        ToolContext::new(
            Arc::new(store),
            Arc::new(MemoryStore::default()),
            Some(Arc::new(shop_graph())),
        )
    }

    #[tokio::test]
    async fn search_finds_revenue_metric_via_alias() {
        let ctx = ctx().await;
        let out = SearchSemanticsTool
            .handle(
                serde_json::json!({ "query": "net revenue mrr by month" }),
                &ctx,
            )
            .await
            .unwrap();
        let items = out["items"].as_array().unwrap();
        assert!(items
            .iter()
            .any(|i| i["id"] == "revenue" && i["kind"] == "metric"));
        assert!(items.len() <= 20);
    }

    #[tokio::test]
    async fn search_finds_time_dimension() {
        let ctx = ctx().await;
        let out = SearchSemanticsTool
            .handle(serde_json::json!({ "query": "order date" }), &ctx)
            .await
            .unwrap();
        assert!(out["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == "order_date"));
    }

    #[tokio::test]
    async fn wrong_source_is_structured_not_found() {
        let ctx = ctx().await;
        let err = SearchSemanticsTool
            .handle(
                serde_json::json!({ "query": "revenue", "source": "nope" }),
                &ctx,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.details.unwrap()["available_source"], "shop");
    }

    #[tokio::test]
    async fn unknown_source_tools_error_structured() {
        let ctx = ctx().await;
        let err = GetSchemaTool
            .handle(serde_json::json!({ "source": "no-such-source" }), &ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    /// End-to-end on the sqlite fixture: revenue-by-month IR → rows + SQL.
    #[tokio::test]
    async fn execute_query_end_to_end_on_sqlite_fixture() {
        let ctx = ctx().await;
        // register the fixture sqlite source
        let dir = std::env::temp_dir().join(format!("querora-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("shop.db");
        let opts = sqlx::sqlite::SqliteConnectOptions::from_str(&format!(
            "sqlite://{}",
            db_path.display()
        ))
        .unwrap()
        .create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::raw_sql(crate::fixtures::SHOP_DDL)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        ctx.store
            .upsert_source(&querora_contracts::SourceInfo {
                id: SourceId::new("shop"),
                name: "Shop".into(),
                kind: querora_contracts::SourceKind::Sqlite,
                params: serde_json::json!({ "path": db_path.display().to_string() }),
                created_at: String::new(),
            })
            .await
            .unwrap();

        let ir = crate::fixtures::revenue_by_month_query();
        let out = ExecuteQueryTool
            .handle(serde_json::json!({ "ir": ir }), &ctx)
            .await
            .unwrap();
        assert!(out["sql"].as_str().unwrap().contains("SUM"));
        assert!(out["sql"].as_str().unwrap().contains("STRFTIME"));
        assert_eq!(out["stats"]["row_count"], serde_json::json!(4)); // Mar/Apr/May/Jun paid
        assert!(out["head"].as_array().unwrap().len() <= 50);
        assert!(!out["result_id"].as_str().unwrap().is_empty());
        // cached full result retrievable app-side
        let full = ctx.results.get(out["result_id"].as_str().unwrap()).unwrap();
        assert_eq!(full.rows.len(), 4);
        assert!(full.sql.contains("LIMIT"));
    }
}
