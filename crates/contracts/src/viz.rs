// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! `VisualizationSpec` — the agent's chart suggestion, mapped onto result
//! columns. The frontend renders it with Vega-Lite; any invalid mapping falls
//! back to a plain table (an agent can never crash the answer).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Chart types M0 knows how to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ChartType {
    /// Vertical bars (categories over time, top-N).
    Bar,
    /// Horizontal bars.
    BarHorizontal,
    /// Time-series line.
    Line,
    /// Time-series area.
    Area,
    /// Share-of-whole.
    Pie,
    /// No chart — table only.
    Table,
}

/// Agent-chosen visualization for a `QueryResult`.
///
/// `x`/`y` reference result column names; the frontend validates that the
/// columns exist and match the chart type, else falls back to `Table`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct VisualizationSpec {
    /// Chart type.
    pub chart_type: ChartType,
    /// X / category / time column name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<String>,
    /// Y / value column name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<String>,
    /// Optional color / split-by column name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Human title for the answer card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}
