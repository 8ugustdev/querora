// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Query results.
//!
//! Two shapes:
//! - [`QueryResult`] — full result, app-side only (UI, result cache).
//! - [`AgentResult`] — truncated (≤ head rows + stats + `result_id`), the
//!   ONLY shape ever returned to an agent. Full rows never enter agent context.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Type of a result column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ColumnMeta {
    /// Integer / whole number.
    Integer,
    /// Floating point / decimal.
    Number,
    /// String / categorical.
    String,
    /// Boolean.
    Boolean,
    /// Date or timestamp (ISO-8601 string in rows).
    Temporal,
}

/// A single result row: column alias → JSON value.
pub type Row = BTreeMap<String, serde_json::Value>;

/// Execution stats for the trust panel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS)]
pub struct ResultStats {
    /// Total rows produced (before head truncation).
    pub row_count: u64,
    /// Query wall time in milliseconds.
    pub duration_ms: u64,
    /// Server-side row cap applied to the executed statement.
    pub row_cap: u32,
    /// Statement timeout applied, in seconds.
    pub timeout_secs: u32,
}

/// Full query result — served to the UI and the app-side result cache only.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct QueryResult {
    /// Cache id; `AgentResult` references it.
    pub result_id: String,
    /// Ordered output column names.
    pub columns: Vec<String>,
    /// Column types, parallel to `columns`.
    pub column_types: Vec<ColumnMeta>,
    /// All rows (≤ row_cap).
    pub rows: Vec<Row>,
    /// The exact SQL that was executed (trust panel).
    pub sql: String,
    /// Bind parameters used (trust panel; values only, no secrets).
    pub params: Vec<serde_json::Value>,
    /// Semantic graph version pinned for this execution.
    pub semantic_version: String,
    /// Execution stats.
    pub stats: ResultStats,
}

/// Truncated result returned to agents. Full rows stay app-side behind
/// `result_id` (rate-limit + context-bloat protection).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct AgentResult {
    /// Key into the app-side result cache (for follow-ups / UI hop-over).
    pub result_id: String,
    /// Ordered output column names.
    pub columns: Vec<String>,
    /// Column types, parallel to `columns`.
    pub column_types: Vec<ColumnMeta>,
    /// First ≤ 50 rows.
    pub head: Vec<Row>,
    /// Stats (total row count, duration, caps).
    pub stats: ResultStats,
    /// The exact SQL that was executed (agent may cite it; it cannot write it).
    pub sql: String,
    /// Semantic graph version pinned for this execution.
    pub semantic_version: String,
}

impl From<&QueryResult> for AgentResult {
    fn from(full: &QueryResult) -> Self {
        const HEAD: usize = 50;
        Self {
            result_id: full.result_id.clone(),
            columns: full.columns.clone(),
            column_types: full.column_types.clone(),
            head: full.rows.iter().take(HEAD).cloned().collect(),
            stats: full.stats,
            sql: full.sql.clone(),
            semantic_version: full.semantic_version.clone(),
        }
    }
}
