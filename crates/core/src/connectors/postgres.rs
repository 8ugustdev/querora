// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! PostgreSQL connector (remote, sqlx). Sessions are forced read-only where
//! the driver supports it; the app-level guard is the primary defense.

use super::guard::assert_single_select;
use super::types::*;
use super::DataSource;
use async_trait::async_trait;
use querora_contracts::{ColumnMeta, ErrorCode, ToolError};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgRow};
use sqlx::{Column, Row, TypeInfo};
use std::time::Duration;

/// Connection to a remote PostgreSQL server.
pub struct PostgresSource {
    pool: PgPool,
}

fn param<'a>(params: &'a serde_json::Value, key: &str) -> Result<&'a str, ToolError> {
    params.get(key).and_then(|v| v.as_str()).ok_or_else(|| {
        ToolError::new(
            ErrorCode::SourceUnavailable,
            format!("postgres source requires `{key}` param"),
        )
    })
}

impl PostgresSource {
    /// Connect. `secret` is the password (Keychain); params carry
    /// host/port/database/user.
    pub async fn connect(params: &serde_json::Value, secret: &str) -> Result<Self, ToolError> {
        let host = param(params, "host")?;
        let db = param(params, "database")?;
        let user = param(params, "user")?;
        let port = params.get("port").and_then(|v| v.as_u64()).unwrap_or(5432) as u16;
        let mut opts = PgConnectOptions::new()
            .host(host)
            .port(port)
            .database(db)
            .username(user)
            .application_name("querora");
        if !secret.is_empty() {
            opts = opts.password(secret);
        }
        // capability probe: fail fast with a structured error
        use sqlx::Connection;
        let _ = sqlx::PgConnection::connect_with(&opts).await.map_err(|e| {
            ToolError::new(
                ErrorCode::SourceUnavailable,
                format!("postgres connect failed: {e}"),
            )
        })?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|e| {
                ToolError::new(
                    ErrorCode::SourceUnavailable,
                    format!("postgres pool failed: {e}"),
                )
            })?;
        Ok(Self { pool })
    }
}

fn err(e: sqlx::Error) -> ToolError {
    ToolError::new(ErrorCode::SourceUnavailable, format!("postgres: {e}"))
}

fn pg_type(t: &str) -> ColumnMeta {
    let t = t.to_lowercase();
    match t.as_str() {
        "int2" | "int4" | "int8" => ColumnMeta::Integer,
        "float4" | "float8" | "numeric" => ColumnMeta::Number,
        "bool" => ColumnMeta::Boolean,
        "date" | "timestamp" | "timestamptz" | "time" => ColumnMeta::Temporal,
        _ => ColumnMeta::String,
    }
}

fn row_to_json(row: &PgRow) -> Vec<serde_json::Value> {
    (0..row.columns().len())
        .map(|i| {
            let t = row.columns()[i].type_info().name().to_lowercase();
            match t.as_str() {
                "int2" => row
                    .try_get::<Option<i16>, _>(i)
                    .ok()
                    .flatten()
                    .map(i64::from)
                    .map(serde_json::Value::from),
                "int4" => row
                    .try_get::<Option<i32>, _>(i)
                    .ok()
                    .flatten()
                    .map(i64::from)
                    .map(serde_json::Value::from),
                "int8" => row
                    .try_get::<Option<i64>, _>(i)
                    .ok()
                    .flatten()
                    .map(serde_json::Value::from),
                "float4" => row
                    .try_get::<Option<f32>, _>(i)
                    .ok()
                    .flatten()
                    .map(f64::from)
                    .map(serde_json::Value::from),
                "float8" => row
                    .try_get::<Option<f64>, _>(i)
                    .ok()
                    .flatten()
                    .map(serde_json::Value::from),
                "numeric" => {
                    row.try_get::<Option<String>, _>(i).ok().flatten().map(|s| {
                        match s.parse::<f64>() {
                            Ok(f) => serde_json::json!(f),
                            Err(_) => serde_json::Value::String(s),
                        }
                    })
                }
                "bool" => row
                    .try_get::<Option<bool>, _>(i)
                    .ok()
                    .flatten()
                    .map(serde_json::Value::from),
                "date" | "timestamp" | "timestamptz" => row
                    .try_get::<Option<chrono::NaiveDateTime>, _>(i)
                    .ok()
                    .flatten()
                    .map(|d| serde_json::Value::String(d.to_string()))
                    .or_else(|| {
                        row.try_get::<Option<chrono::NaiveDate>, _>(i)
                            .ok()
                            .flatten()
                            .map(|d| serde_json::Value::String(d.to_string()))
                    }),
                _ => row
                    .try_get::<Option<String>, _>(i)
                    .ok()
                    .flatten()
                    .map(serde_json::Value::from),
            }
            .unwrap_or(serde_json::Value::Null)
        })
        .collect()
}

#[async_trait]
impl DataSource for PostgresSource {
    async fn catalog(&self) -> Result<DatabaseCatalog, ToolError> {
        let cols: Vec<(String, String, String, String, bool)> = sqlx::query_as(
            "SELECT c.table_name, t.table_type, c.column_name, c.data_type, c.is_nullable = 'YES'
             FROM information_schema.columns c
             JOIN information_schema.tables t
               ON t.table_name = c.table_name AND t.table_schema = c.table_schema
             WHERE c.table_schema = 'public'
             ORDER BY c.table_name, c.ordinal_position",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(err)?;

        // PKs and FKs
        let pks: Vec<(String, String)> = sqlx::query_as(
            "SELECT kcu.table_name, kcu.column_name
             FROM information_schema.table_constraints tc
             JOIN information_schema.key_column_usage kcu
               ON kcu.constraint_name = tc.constraint_name AND kcu.table_schema = tc.table_schema
             WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = 'public'",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(err)?;
        let fks: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT tc.constraint_name, kcu.table_name, kcu.column_name,
                    ccu.table_name AS ref_table, ccu.column_name AS ref_column
             FROM information_schema.table_constraints tc
             JOIN information_schema.key_column_usage kcu
               ON kcu.constraint_name = tc.constraint_name AND kcu.table_schema = tc.table_schema
             JOIN information_schema.constraint_column_usage ccu
               ON ccu.constraint_name = tc.constraint_name AND ccu.table_schema = tc.table_schema
             WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_schema = 'public'",
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
                nullable,
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
        super::profile::profile_pg(&self.pool, table, column, sample).await
    }

    async fn execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        cap: RowCap,
    ) -> Result<RawRows, ToolError> {
        let sql = assert_single_select(sql)?;
        // session read-only hardening (defense-in-depth)
        let mut tx = self.pool.begin().await.map_err(err)?;
        sqlx::query("SET default_transaction_read_only = on")
            .execute(&mut *tx)
            .await
            .map_err(err)?;
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
        let fut = q.fetch_all(&mut *tx);
        let rows: Vec<PgRow> =
            tokio::time::timeout(Duration::from_secs(cap.timeout_secs as u64), fut)
                .await
                .map_err(|_| ToolError::new(ErrorCode::SourceUnavailable, "query timeout"))?
                .map_err(err)?;
        tx.rollback().await.ok();
        let limited: Vec<PgRow> = rows.into_iter().take(cap.limit as usize).collect();
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
                        .map(|c| pg_type(c.type_info().name()))
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
        Dialect::Pg
    }
}
