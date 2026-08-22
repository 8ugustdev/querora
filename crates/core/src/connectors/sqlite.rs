// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! SQLite file connector (local source). Read-only open; powers
//! dogfooding and the Phase 5 driver fixtures.

use super::guard::assert_single_select;
use super::types::*;
use super::DataSource;
use async_trait::async_trait;
use querora_contracts::{ColumnMeta, ErrorCode, ToolError};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{Column, Row, TypeInfo};
use std::str::FromStr;
use std::time::Duration;

/// Connection to a local SQLite database file.
pub struct SqliteSource {
    pool: SqlitePool,
}

fn param_str<'a>(params: &'a serde_json::Value, key: &str) -> Result<&'a str, ToolError> {
    params.get(key).and_then(|v| v.as_str()).ok_or_else(|| {
        ToolError::new(
            ErrorCode::SourceUnavailable,
            format!("sqlite source requires `{key}` param"),
        )
    })
}

impl SqliteSource {
    /// Open read-only. WAL side files are still honored for reads.
    pub async fn connect(params: &serde_json::Value, _secret: &str) -> Result<Self, ToolError> {
        let path = param_str(params, "path")?;
        if !std::path::Path::new(path).exists() {
            return Err(ToolError::new(
                ErrorCode::SourceUnavailable,
                format!("sqlite file not found: {path}"),
            ));
        }
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))
            .map_err(|e| ToolError::new(ErrorCode::SourceUnavailable, e.to_string()))?
            .read_only(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|e| {
                ToolError::new(
                    ErrorCode::SourceUnavailable,
                    format!("sqlite open failed: {e}"),
                )
            })?;
        Ok(Self { pool })
    }

    async fn catalog(&self) -> Result<DatabaseCatalog, ToolError> {
        let tables: Vec<(String, String)> = sqlx::query_as(
            "SELECT name, type FROM sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(err)?;
        let mut out = Vec::new();
        for (name, ty) in tables {
            let cols: Vec<(String, String, i64, i64)> = sqlx::query_as(&format!(
                "SELECT name, type, \"notnull\", pk FROM pragma_table_info('{name}')"
            ))
            .fetch_all(&self.pool)
            .await
            .map_err(err)?;
            let fks: Vec<(String, String, String)> = sqlx::query_as(&format!(
                "SELECT \"table\", \"from\", \"to\" FROM pragma_foreign_key_list('{name}')"
            ))
            .fetch_all(&self.pool)
            .await
            .map_err(err)?;
            out.push(TableInfo {
                name: name.clone(),
                is_view: ty == "view",
                columns: cols
                    .iter()
                    .map(|(cn, ct, notnull, pk)| ColumnInfo {
                        name: cn.clone(),
                        data_type: ct.clone(),
                        nullable: *notnull == 0,
                        primary_key: *pk > 0,
                    })
                    .collect(),
                foreign_keys: fks
                    .iter()
                    .map(|(rt, fc, tc)| ForeignKey {
                        name: format!("{name}_{fc}_fkey"),
                        column: fc.clone(),
                        ref_table: rt.clone(),
                        ref_column: tc.clone(),
                    })
                    .collect(),
            });
        }
        Ok(DatabaseCatalog { tables: out })
    }
}

fn err(e: sqlx::Error) -> ToolError {
    ToolError::new(ErrorCode::SourceUnavailable, format!("sqlite: {e}"))
}

pub(super) fn row_to_json(row: &SqliteRow) -> Vec<serde_json::Value> {
    (0..row.columns().len())
        .map(|i| match row.try_get::<Option<i64>, _>(i) {
            Ok(v) => v
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null),
            Err(_) => match row.try_get::<Option<f64>, _>(i) {
                Ok(v) => v
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null),
                Err(_) => match row.try_get::<Option<String>, _>(i) {
                    Ok(v) => v
                        .map(serde_json::Value::from)
                        .unwrap_or(serde_json::Value::Null),
                    Err(_) => match row.try_get::<Option<bool>, _>(i) {
                        Ok(v) => v
                            .map(serde_json::Value::from)
                            .unwrap_or(serde_json::Value::Null),
                        Err(_) => serde_json::Value::Null,
                    },
                },
            },
        })
        .collect()
}

pub(super) fn sqlite_type(t: &str) -> ColumnMeta {
    let t = t.to_lowercase();
    if t.contains("int") {
        ColumnMeta::Integer
    } else if t.contains("real")
        || t.contains("floa")
        || t.contains("doub")
        || t.contains("num")
        || t.contains("dec")
    {
        ColumnMeta::Number
    } else if t.contains("bool") {
        ColumnMeta::Boolean
    } else if t.contains("date") || t.contains("time") {
        ColumnMeta::Temporal
    } else {
        ColumnMeta::String
    }
}

#[async_trait]
impl DataSource for SqliteSource {
    async fn catalog(&self) -> Result<DatabaseCatalog, ToolError> {
        self.catalog().await
    }

    async fn profile(
        &self,
        table: &str,
        column: &str,
        sample: u64,
    ) -> Result<ColumnProfile, ToolError> {
        super::profile::profile_sqlite(&self.pool, table, column, sample).await
    }

    async fn execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        cap: RowCap,
    ) -> Result<RawRows, ToolError> {
        let sql = assert_single_select(sql)?;
        let mut q = sqlx::query(&sql);
        for p in params {
            q = bind_json(q, p);
        }
        let fut = q.fetch_all(&self.pool);
        let rows: Vec<SqliteRow> =
            tokio::time::timeout(Duration::from_secs(cap.timeout_secs as u64), fut)
                .await
                .map_err(|_| ToolError::new(ErrorCode::SourceUnavailable, "query timeout"))?
                .map_err(err)?;
        let truncated = rows.len() > cap.limit as usize;
        let limited: Vec<SqliteRow> = rows.into_iter().take(cap.limit as usize + 1).collect();
        let (columns, column_types) = limited
            .first()
            .map(|r| {
                (
                    r.columns()
                        .iter()
                        .map(|c| c.name().to_string())
                        .collect::<Vec<_>>(),
                    r.columns()
                        .iter()
                        .map(|c| sqlite_type(c.type_info().name()))
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default();
        Ok(RawRows {
            columns,
            column_types,
            rows: limited.iter().map(row_to_json).collect(),
            row_cap: cap.limit,
        })
        .map(|mut rr| {
            if truncated {
                rr.rows.truncate(cap.limit as usize);
            }
            rr
        })
    }

    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }
}

/// Bind a JSON value to a sqlx sqlite query.
pub(super) fn bind_json<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    v: &serde_json::Value,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    match v {
        serde_json::Value::Null => q.bind(Option::<String>::None),
        serde_json::Value::Bool(b) => q.bind(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                q.bind(i)
            } else {
                q.bind(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => q.bind(s.clone()),
        other => q.bind(other.to_string()),
    }
}
