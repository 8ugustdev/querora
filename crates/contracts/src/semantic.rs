// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! `SemanticGraph` — the human-published business vocabulary.
//!
//! Entities map to tables, metrics to aggregation expressions, dimensions to
//! columns. The compiler accepts queries ONLY against a published graph;
//! drafts (heuristic or AI-enriched) must go through review → publish first.

use crate::source::SourceId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Physical/logical data types relevant to analytics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum SemanticDataType {
    /// Integer / whole number.
    Integer,
    /// Floating point / decimal.
    Number,
    /// String / categorical.
    String,
    /// Boolean.
    Boolean,
    /// Date or timestamp.
    Temporal,
}

/// How confident Querora is in a suggested graph element.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Inferred from naming conventions or value overlap — needs review.
    Candidate,
    /// Declared FK / explicit metadata — trusted.
    Declared,
}

/// Relationship cardinality (M0 models many-to-one from fact to dimension).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum JoinCardinality {
    /// Many rows on the `from` side map to one row on the `to` side.
    ManyToOne,
    /// One-to-one.
    OneToOne,
}

/// How a relationship renders in SQL.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum JoinKind {
    /// LEFT JOIN — safe when the right side has ≤ 1 row per left row.
    #[default]
    Join,
    /// SEMI-JOIN — filters on the related entity render as
    /// `col IN (SELECT …)` — no row multiplication. Required for
    /// many-to-many (e.g. product ↔ category).
    Semi,
}

/// A joinable relationship between two entities.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct Relationship {
    /// Unique relationship id (slug).
    pub id: String,
    /// Entity id on the "many" side.
    pub from_entity: String,
    /// Join column on the from side (usually the FK column).
    pub from_column: String,
    /// Entity id on the "one" side.
    pub to_entity: String,
    /// Join column on the to side (usually the PK).
    pub to_column: String,
    /// Cardinality.
    pub cardinality: JoinCardinality,
    /// Where this relationship came from.
    pub confidence: Confidence,
    /// Join rendering: full LEFT JOIN or semi-join (filter-only).
    #[serde(default)]
    pub join_kind: JoinKind,
}

/// Aggregation operators for metric expressions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum AggOp {
    /// `SUM(expr)`
    Sum,
    /// `AVG(expr)`
    Avg,
    /// `MIN(expr)`
    Min,
    /// `MAX(expr)`
    Max,
    /// `COUNT(expr)`
    Count,
    /// `COUNT(DISTINCT expr)`
    CountDistinct,
    /// Ratio of two measures: `sum(numerator) / NULLIF(sum(denominator), 0)`.
    Ratio {
        /// Metric id of the numerator.
        numerator: String,
        /// Metric id of the denominator.
        denominator: String,
    },
}

/// Arithmetic operators for metric expression combinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ArithOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/` (right side NULL-guarded by the renderer)
    Div,
}

/// The aggregation expression behind a metric. References physical columns
/// of the entity — the agent never sees or writes this; it uses metric ids.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct MetricExpr {
    /// Aggregation operator.
    pub op: AggOp,
    /// Column expression (column name; richer SQL expressions post-M0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    /// Human-facing formula, trust-panel only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_formula: Option<String>,
    /// Arithmetic combination (e.g. margin = (revenue − cost) / revenue).
    /// When set, `op`/`column` are ignored. Both sides must aggregate
    /// columns of the SAME entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combination: Option<Combination>,
}

/// Arithmetic combination of two metric sub-expressions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct Combination {
    /// Left operand.
    pub left: Box<MetricExpr>,
    /// Operator.
    pub op: ArithOp,
    /// Right operand.
    pub right: Box<MetricExpr>,
}

/// A business metric.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct Metric {
    /// Unique metric id (referenced by `MeasureRef`).
    pub id: String,
    /// Human label.
    pub label: String,
    /// Entity the metric's columns live on.
    pub entity_id: String,
    /// Aggregation expression.
    pub expr: MetricExpr,
    /// Searchable aliases ("revenue", "mrr", …).
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Short description surfaced to agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A business dimension (a groupable column).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct Dimension {
    /// Unique dimension id (referenced by `DimensionRef` / `Filter`).
    pub id: String,
    /// Human label.
    pub label: String,
    /// Entity the dimension lives on.
    pub entity_id: String,
    /// Physical column name.
    pub column: String,
    /// Data type.
    pub data_type: SemanticDataType,
    /// Searchable aliases.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Short description surfaced to agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// An entity: the business name of a table (or view).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct Entity {
    /// Unique entity id (slug of the business name).
    pub id: String,
    /// Human label.
    pub label: String,
    /// Physical table name. For virtual entities this is the CTE name
    /// (derived from the id) — ignored when `definition_sql` is set.
    pub table: String,
    /// Short description surfaced to agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Virtual entity: a single SELECT (validated read-only at publish)
    /// whose output columns act as this entity's columns. Rendered as a
    /// CTE. Authored in the semantic layer (human/AI-published) — agents
    /// still never write SQL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_sql: Option<String>,
}

/// The whole semantic graph for one source, at one version.
///
/// `version` is the immutable published version id; queries are pinned to it
/// at execution time so the trust panel can show exactly what was used.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct SemanticGraph {
    /// Source this graph describes.
    pub source: SourceId,
    /// Immutable version tag (set on publish; empty in drafts).
    pub version: String,
    /// `true` once a human has published this graph.
    pub published: bool,
    /// Entities by id.
    pub entities: BTreeMap<String, Entity>,
    /// Metrics by id.
    pub metrics: BTreeMap<String, Metric>,
    /// Dimensions by id.
    pub dimensions: BTreeMap<String, Dimension>,
    /// Relationships (join graph).
    pub relationships: Vec<Relationship>,
    /// Sample dimension values for value-aware search (dim id → values).
    /// Populated by the EAV pass / curated manually; indexed into FTS.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub value_index: BTreeMap<String, Vec<String>>,
}
