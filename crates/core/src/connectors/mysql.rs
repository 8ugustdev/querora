// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! MySQL 8.x connector (remote, sqlx). Test matrix is 8.x only (M0).

use super::guard::assert_single_select;
use super::types::*;
use super::DataSource;
use async_trait::async_trait;
use querora_contracts::{ColumnMeta, ErrorCode, ToolError};
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::{Column, Row, TypeInfo};
use std::time::Duration;

/// Connection to a remote MySQL server.
pub struct MysqlSource {
    pool: MySqlPool,
}

fn param<'a>(params: &'a serde_json::Value, key: &str) -> Result<&'a str, ToolError> {
    params.get(key).and_then(|v| v.as_str()).ok_or_else(|| {
        ToolError::new(
            ErrorCode::SourceUnavailable,
            format!("mysql source requires `{key}` param"),
        )
    })
}

impl MysqlSource {
    /// Connect. `secret` is the password (Keychain).
    pub async fn connect(params: &serde_json::Value, secret: &str) -> Result<Self, ToolError> {
        let host = param(params, "host")?;
        let db = param(params, "database")?;
        let user = param(params, "user")?;
        let port = params.get("port").and_then(|v| v.as_u64()).unwrap_or(3306) as u16;
        let opts = MySqlConnectOptions::new()
            .host(host)
            .port(port)
            .database(db)
            .username(user)
            .password(secret);
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|e| {
                ToolError::new(
                    ErrorCode::SourceUnavailable,
                    format!("mysql connect failed: {e}"),
                )
            })?;
        Ok(Self { pool })
    }
}

fn err(e: sqlx::Error) -> ToolError {
    ToolError::new(ErrorCode::SourceUnavailable, format!("mysql: {e}"))
}

fn mysql_type(t: &str) -> ColumnMeta {
    let t = t.to_lowercase();
    if t.contains("int") {
        ColumnMeta::Integer
    } else if t.contains("double")
        || t.contains("float")
        || t.contains("decimal")
        || t.contains("numeric")
    {
        ColumnMeta::Number
    } else if t.contains("bit(1)") || t == "bool" || t == "boolean" {
        ColumnMeta::Boolean
    } else if t.contains("date") || t.contains("time") {
        ColumnMeta::Temporal
    } else {
        ColumnMeta::String
    }
}

fn row_to_json(row: &MySqlRow) -> Vec<serde_json::Value> {
    // Type-blind decode cascade (driver type names vary: "int", "int
    // unsigned", "newdecimal", …) — first successful typed decode wins.
    (0..row.columns().len())
        .map(|i| {
            let t = row.columns()[i].type_info().name().to_lowercase();
            if t.contains("int")
                || t.contains("decimal")
                || t.contains("double")
                || t.contains("float")
                || t.contains("numeric")
            {
                row.try_get::<Option<rust_decimal::Decimal>, _>(i)
                    .ok()
                    .flatten()
                    .map(|d| serde_json::json!(d.mantissa() as f64 / 10f64.powi(d.scale() as i32)))
                    .or_else(|| {
                        row.try_get::<Option<i64>, _>(i)
                            .ok()
                            .flatten()
                            .map(serde_json::Value::from)
                    })
                    .or_else(|| {
                        row.try_get::<Option<u64>, _>(i)
                            .ok()
                            .flatten()
                            .map(serde_json::Value::from)
                    })
                    .or_else(|| {
                        row.try_get::<Option<i32>, _>(i)
                            .ok()
                            .flatten()
                            .map(i64::from)
                            .map(serde_json::Value::from)
                    })
                    .or_else(|| {
                        row.try_get::<Option<u32>, _>(i)
                            .ok()
                            .flatten()
                            .map(i64::from)
                            .map(serde_json::Value::from)
                    })
                    .or_else(|| {
                        row.try_get::<Option<i16>, _>(i)
                            .ok()
                            .flatten()
                            .map(i64::from)
                            .map(serde_json::Value::from)
                    })
                    .or_else(|| {
                        row.try_get::<Option<u16>, _>(i)
                            .ok()
                            .flatten()
                            .map(i64::from)
                            .map(serde_json::Value::from)
                    })
                    .or_else(|| {
                        row.try_get::<Option<u8>, _>(i)
                            .ok()
                            .flatten()
                            .map(i64::from)
                            .map(serde_json::Value::from)
                    })
                    .or_else(|| {
                        row.try_get::<Option<f64>, _>(i)
                            .ok()
                            .flatten()
                            .map(serde_json::Value::from)
                    })
                    .or_else(|| {
                        row.try_get::<Option<f32>, _>(i)
                            .ok()
                            .flatten()
                            .map(f64::from)
                            .map(serde_json::Value::from)
                    })
                    .or_else(|| {
                        row.try_get::<Option<String>, _>(i)
                            .ok()
                            .flatten()
                            .and_then(|v| v.parse::<f64>().ok())
                            .map(serde_json::Value::from)
                    })
                    .unwrap_or(serde_json::Value::Null)
            } else if t.contains("bit") || t == "bool" || t == "boolean" {
                row.try_get::<Option<bool>, _>(i)
                    .ok()
                    .flatten()
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null)
            } else if t.contains("date") || t.contains("time") {
                row.try_get::<Option<chrono::NaiveDateTime>, _>(i)
                    .ok()
                    .flatten()
                    .map(|d| serde_json::Value::String(d.to_string()))
                    .or_else(|| {
                        row.try_get::<Option<chrono::NaiveDate>, _>(i)
                            .ok()
                            .flatten()
                            .map(|d| serde_json::Value::String(d.to_string()))
                    })
                    .unwrap_or(serde_json::Value::Null)
            } else {
                row.try_get::<Option<String>, _>(i)
                    .ok()
                    .flatten()
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null)
            }
        })
        .collect()
}

#[async_trait]
impl DataSource for MysqlSource {
    async fn catalog(&self) -> Result<DatabaseCatalog, ToolError> {
        let cols: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT CAST(t.table_name AS CHAR), CAST(t.table_type AS CHAR), CAST(c.column_name AS CHAR), CAST(c.data_type AS CHAR),
                    CAST(c.is_nullable AS CHAR)
             FROM information_schema.columns c
             JOIN information_schema.tables t
               ON t.table_schema = c.table_schema AND t.table_name = c.table_name
             WHERE c.table_schema = DATABASE()
             ORDER BY c.table_name, c.ordinal_position",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(err)?;

        let pks: Vec<(String, String)> = sqlx::query_as(
            "SELECT CAST(table_name AS CHAR), CAST(column_name AS CHAR)
             FROM information_schema.statistics
             WHERE table_schema = DATABASE() AND index_name = 'PRIMARY'
             ORDER BY table_name, seq_in_index",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(err)?;

        let fks: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT CAST(kcu.constraint_name AS CHAR), CAST(kcu.table_name AS CHAR), CAST(kcu.column_name AS CHAR),
                    CAST(kcu.referenced_table_name AS CHAR), CAST(kcu.referenced_column_name AS CHAR)
             FROM information_schema.key_column_usage kcu
             JOIN information_schema.table_constraints tc
               ON tc.constraint_name = kcu.constraint_name
              AND tc.table_schema = kcu.table_schema
              AND tc.constraint_type = 'FOREIGN KEY'
             WHERE kcu.table_schema = DATABASE() AND kcu.referenced_table_name IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(err)?;

        let mut tables: std::collections::BTreeMap<String, TableInfo> = Default::default();
        for (table, ty, column, dtype, nullable) in cols {
            let entry = tables.entry(table.clone()).or_insert_with(|| TableInfo {
                name: table,
                is_view: ty == "VIEW",
                columns: vec![],
                foreign_keys: vec![],
            });
            entry.columns.push(ColumnInfo {
                name: column,
                data_type: dtype,
                nullable: nullable == "YES",
                primary_key: false,
            });
        }
        for (table, column) in pks {
            if let Some(t) = tables.get_mut(&table) {
                for c in &mut t.columns {
                    if c.name == column {
                        c.primary_key = true;
                    }
                }
            }
        }
        for (_name, table, column, ref_table, ref_column) in fks {
            if let Some(t) = tables.get_mut(&table) {
                t.foreign_keys.push(ForeignKey {
                    name: format!("{table}_{column}_fkey"),
                    column,
                    ref_table,
                    ref_column,
                });
            }
        }
        Ok(DatabaseCatalog {
            tables: tables.into_values().collect(),
        })
    }

    async fn profile(
        &self,
        table: &str,
        column: &str,
        sample: u64,
    ) -> Result<ColumnProfile, ToolError> {
        super::profile::profile_mysql(&self.pool, table, column, sample).await
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
            q = match p {
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
            };
        }
        let fut = q.fetch_all(&self.pool);
        let rows: Vec<MySqlRow> =
            tokio::time::timeout(Duration::from_secs(cap.timeout_secs as u64), fut)
                .await
                .map_err(|_| ToolError::new(ErrorCode::SourceUnavailable, "query timeout"))?
                .map_err(err)?;
        let limited: Vec<MySqlRow> = rows.into_iter().take(cap.limit as usize).collect();
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
                        .map(|c| mysql_type(c.type_info().name()))
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
    }

    fn dialect(&self) -> Dialect {
        Dialect::Mysql
    }
}
