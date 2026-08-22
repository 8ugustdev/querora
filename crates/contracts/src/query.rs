// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! `AnalyticalQuery` — the intermediate representation agents emit.
//!
//! An agent NEVER writes SQL. It composes an `AnalyticalQuery` out of ids
//! published in the `SemanticGraph` (metric ids, dimension ids); the compiler
//! (Phase 4) validates it against the published graph and renders dialect SQL.

use crate::source::SourceId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Reference to a published metric, with an optional output alias.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct MeasureRef {
    /// Metric id from the published semantic graph.
    pub metric_id: String,
    /// Output column alias; defaults to the metric label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

/// Reference to a published dimension, optionally at a time grain.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct DimensionRef {
    /// Dimension id from the published semantic graph.
    pub dimension_id: String,
    /// Time grain when the dimension is temporal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<TimeGrain>,
    /// Output column alias; defaults to the dimension label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

/// Comparison operators available in filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    /// `=`
    Eq,
    /// `<>`
    NotEq,
    /// `IN (…)`
    In,
    /// `NOT IN (…)`
    NotIn,
    /// `>`
    Gt,
    /// `>=`
    Gte,
    /// `<`
    Lt,
    /// `<=`
    Lte,
    /// `LIKE` with escaped pattern
    Like,
    /// `NOT LIKE` with escaped pattern
    NotLike,
    /// `IS NULL`
    IsNull,
    /// `IS NOT NULL`
    IsNotNull,
}

/// A literal filter value. Always parameterized in rendered SQL —
/// user values are never string-interpolated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum FilterValue {
    /// JSON null (distinct from SQL NULL only in transport).
    Null,
    /// Boolean literal.
    Bool(bool),
    /// Numeric literal.
    Number(f64),
    /// String literal.
    Str(String),
    /// List of literals for `in` / `not_in`.
    List(Vec<FilterValue>),
}

/// A filter on a published dimension.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct Filter {
    /// Dimension id from the published semantic graph.
    pub dimension_id: String,
    /// Comparison operator.
    pub op: FilterOp,
    /// Literal(s) to compare against (ignored for `is_null` / `is_not_null`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<FilterValue>,
}

/// Time bucketing grains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum TimeGrain {
    /// Calendar day.
    Day,
    /// Calendar week.
    Week,
    /// Calendar month.
    Month,
    /// Calendar quarter.
    Quarter,
    /// Calendar year.
    Year,
}

/// Units for relative time ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum TimeUnit {
    /// Days.
    Day,
    /// Weeks.
    Week,
    /// Months.
    Month,
    /// Quarters.
    Quarter,
    /// Years.
    Year,
}

/// Time range on a temporal dimension. Relative ranges are resolved at
/// compile time; the resolved absolute bounds are echoed in the ExecutionPlan.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum TimeRange {
    /// Last N units ending now (inclusive of the current partial bucket).
    Last { count: u32, unit: TimeUnit },
    /// Absolute range, ISO-8601 dates (inclusive).
    Between { start: String, end: String },
}

/// Optional period-over-period comparison mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum CompareMode {
    /// Compute the same query for the immediately preceding period of the
    /// same length (e.g. last 6 months vs the 6 months before).
    PreviousPeriod,
}

/// Time context of a query: which temporal dimension anchors it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct TimeSpec {
    /// Temporal dimension id from the published semantic graph.
    pub dimension_id: String,
    /// Range restriction.
    pub range: TimeRange,
    /// When set, ALSO compute the comparison period and return aligned
    /// rows tagged `period ∈ {current, previous}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare: Option<CompareMode>,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum OrderDirection {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

/// Ordering on an output column (dimension or measure alias/metric id).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct OrderSpec {
    /// Metric id, dimension id, or output alias.
    pub key: String,
    /// Sort direction.
    pub direction: OrderDirection,
}

/// The analytical IR. Agents compose this via tools; the compiler validates
/// it against the published `SemanticGraph` and renders SQL.
///
/// The `limit` is capped by app config (default 1000) — never trusted.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct AnalyticalQuery {
    /// Target source.
    pub source: SourceId,
    /// Metrics to compute.
    #[serde(default)]
    pub measures: Vec<MeasureRef>,
    /// Dimensions to group by.
    #[serde(default)]
    pub dimensions: Vec<DimensionRef>,
    /// Filters.
    #[serde(default)]
    pub filters: Vec<Filter>,
    /// Optional time context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeSpec>,
    /// Ordering.
    #[serde(default)]
    pub order: Vec<OrderSpec>,
    /// Requested row limit (server-capped).
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    100
}
