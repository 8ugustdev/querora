// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! # querora-contracts
//!
//! The single source of truth for all shared data contracts in Querora.
//! Every other crate — `core`, `mcp`, the desktop app, the pi sidecar (via
//! generated TypeScript) — depends on **this** crate; `contracts` depends on
//! nothing Querora-internal. Hexagonal rule: all dependencies point here.
//!
//! - [`query`]: `AnalyticalQuery` — the IR an agent emits. Never SQL.
//! - [`semantic`]: `SemanticGraph` — entities, relationships, metrics, dimensions.
//! - [`result`]: `QueryResult` (full, app-side) and `AgentResult` (truncated, agent-side).
//! - [`viz`]: `VisualizationSpec` — agent-chosen chart mapping.
//! - [`source`]: `SourceId`, `SourceKind`, public `SourceInfo` (never credentials).
//! - [`error`]: `ToolError` — structured errors agents can self-correct from.

pub mod agent;
pub mod error;
pub mod query;
pub mod result;
pub mod semantic;
pub mod source;
pub mod viz;

pub use agent::{AgentEvent, AgentStatus};
pub use error::{ErrorCode, ToolError};
pub use query::{
    AnalyticalQuery, CompareMode, DimensionRef, Filter, FilterOp, FilterValue, MeasureRef,
    OrderDirection, OrderSpec, TimeGrain, TimeRange, TimeSpec, TimeUnit,
};
pub use result::{AgentResult, ColumnMeta, QueryResult, ResultStats, Row};
pub use semantic::{
    AggOp, Confidence, Dimension, Entity, JoinCardinality, Metric, MetricExpr, Relationship,
    SemanticDataType, SemanticGraph,
};
pub use source::{SourceId, SourceInfo, SourceKind};
pub use viz::{ChartType, VisualizationSpec};

/// TypeScript bindings export target (relative to this crate's manifest).
#[cfg(test)]
mod ts_export_tests {
    use crate::*;
    use ts_rs::TS;

    /// Export every contract type to the desktop app's generated types dir.
    /// Run via `cargo test -p querora-contracts`; output is committed.
    #[test]
    fn export_bindings() {
        let dir = "../../apps/desktop/src/lib/types/";
        <AnalyticalQuery as TS>::export_all_to(dir).unwrap();
        <SemanticGraph as TS>::export_all_to(dir).unwrap();
        <QueryResult as TS>::export_all_to(dir).unwrap();
        <AgentResult as TS>::export_all_to(dir).unwrap();
        <VisualizationSpec as TS>::export_all_to(dir).unwrap();
        <SourceInfo as TS>::export_all_to(dir).unwrap();
        <ToolError as TS>::export_all_to(dir).unwrap();
        <AgentEvent as TS>::export_all_to(dir).unwrap();
        <AgentStatus as TS>::export_all_to(dir).unwrap();
        // standalones not covered above (Row inlines into QueryResult/AgentResult)
        <FilterValue as TS>::export_all_to(dir).unwrap();
        <TimeRange as TS>::export_all_to(dir).unwrap();
    }
}
