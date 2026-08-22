// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! The IR→SQL compiler: the "no text-to-SQL" core.
//!
//! ```text
//! AnalyticalQuery
//!   → SemanticValidator   (graph lookup; structured, correctable errors)
//!   → Planner             (base entity + BFS join paths, cycle guard)
//!   → DialectRenderer     (quoting, time grains, parameterized filters)
//!   → ExecutionPlan { sql, params, timeout, row_cap }
//! ```
//! The compiler owns SQL, limits, and safety — agents only ever emit IR.

pub mod plan;
pub mod render;
pub mod validate;

use crate::connectors::types::Dialect;
pub use plan::{ExecutionPlan, JoinStep, ResolvedMeasure, ResolvedQuery, SelectItem, SqlCondition};
use querora_contracts::{AnalyticalQuery, ErrorCode, SemanticGraph, ToolError};

/// Compile `query` against `graph` for `dialect`.
pub fn compile(
    query: &AnalyticalQuery,
    graph: &SemanticGraph,
    dialect: Dialect,
) -> Result<ExecutionPlan, ToolError> {
    let resolved = validate::validate(query, graph)?;
    // virtual entities: definitions must be single read-only SELECTs
    for e in graph.entities.values() {
        if let Some(def) = &e.definition_sql {
            crate::connectors::guard::assert_single_select(def).map_err(|err| {
                ToolError::new(
                    ErrorCode::InvalidIr,
                    format!(
                        "virtual entity `{}` has an invalid definition: {}",
                        e.id, err.message
                    ),
                )
            })?;
        }
    }
    let planned = plan::plan(resolved, graph)?;
    render::render(&planned, dialect)
}

/// Compile an EXPLAIN-only plan (dry-run tool): same pipeline, wrapped in
/// the dialect's explain prefix. Nothing is executed.
pub fn compile_explain(
    query: &AnalyticalQuery,
    graph: &SemanticGraph,
    dialect: Dialect,
) -> Result<ExecutionPlan, ToolError> {
    let mut plan = compile(query, graph, dialect)?;
    plan.sql = format!("{}{}", render::explain_prefix(dialect), plan.sql);
    plan.explain_only = true;
    Ok(plan)
}
