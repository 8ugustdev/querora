// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! `SourceId` and public source descriptors.
//!
//! Credentials are **never** part of any type in this module — they live in
//! the macOS Keychain and are injected only inside the connector layer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Stable identifier of a connected data source (slug).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
pub struct SourceId(pub String);

impl SourceId {
    /// Create a new source id from a string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Supported source kinds (v0: local + remote per locked decision #4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Remote PostgreSQL server.
    Postgres,
    /// Remote MySQL 8.x server.
    Mysql,
    /// Local SQLite database file.
    Sqlite,
    /// Local DuckDB file / Parquet / CSV directory.
    DuckDb,
}

/// Public description of a connected source. `params` carries only
/// non-secret connection info (host, port, database, file path…).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct SourceInfo {
    pub id: SourceId,
    pub name: String,
    pub kind: SourceKind,
    /// Non-secret connection parameters, e.g. `{"host": …, "port": …}`.
    #[serde(default)]
    pub params: serde_json::Value,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
}
