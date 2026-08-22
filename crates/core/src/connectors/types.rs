// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Shared connector types: catalogs, profiles, raw rows, dialects, row caps.

use querora_contracts::ColumnMeta;
use serde::{Deserialize, Serialize};

/// SQL dialects the compiler can render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    /// PostgreSQL.
    Pg,
    /// MySQL 8.x.
    Mysql,
    /// SQLite.
    Sqlite,
    /// DuckDB.
    DuckDb,
}

/// One column of a table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnInfo {
    /// Column name.
    pub name: String,
    /// Raw DB type (e.g. `varchar(255)`, `INTEGER`).
    pub data_type: String,
    /// Nullable?
    pub nullable: bool,
    /// Part of the primary key?
    pub primary_key: bool,
}

/// One table (or view) in the catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfo {
    /// Table name.
    pub name: String,
    /// true when this is a view.
    pub is_view: bool,
    /// Columns in ordinal order.
    pub columns: Vec<ColumnInfo>,
    /// Declared foreign keys.
    pub foreign_keys: Vec<ForeignKey>,
}

/// A declared foreign-key relationship.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKey {
    /// Constraint name when available.
    pub name: String,
    /// Local column.
    pub column: String,
    /// Referenced table.
    pub ref_table: String,
    /// Referenced column.
    pub ref_column: String,
}

/// Full catalog snapshot of a source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatabaseCatalog {
    /// Tables (and views).
    pub tables: Vec<TableInfo>,
}

/// Statistics of one column (sampled; LIMIT-based, never a full scan).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColumnProfile {
    /// Approximate distinct count (from sample).
    pub distinct_count: Option<u64>,
    /// Null ratio 0.0–1.0 (from sample).
    pub null_ratio: Option<f64>,
    /// Min value (typed JSON).
    pub min: Option<serde_json::Value>,
    /// Max value (typed JSON).
    pub max: Option<serde_json::Value>,
    /// Top values with counts (categoricals mostly).
    pub top_values: Vec<(String, u64)>,
    /// For temporal columns: min/max as ISO dates.
    pub time_range: Option<(String, String)>,
    /// Sample size used.
    pub sampled_rows: u64,
}

/// A fetched row: values in column order (JSON-typed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawRows {
    /// Column names.
    pub columns: Vec<String>,
    /// Column types.
    pub column_types: Vec<ColumnMeta>,
    /// Rows of JSON values.
    pub rows: Vec<Vec<serde_json::Value>>,
    /// Row cap that was applied.
    pub row_cap: u32,
}

/// What to profile.
#[derive(Debug, Clone)]
pub enum ProfileTarget {
    /// One column of one table.
    Column { table: String, column: String },
    /// Every column of one table.
    Table { table: String },
}

/// Execution limits. `row_cap` is injected by the compiler; the guard
/// enforces statement shape.
#[derive(Debug, Clone, Copy)]
pub struct RowCap {
    /// Max rows returned.
    pub limit: u32,
    /// Statement timeout in seconds.
    pub timeout_secs: u32,
}

impl Default for RowCap {
    fn default() -> Self {
        Self {
            limit: 1000,
            timeout_secs: 30,
        }
    }
}
