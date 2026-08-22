// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! DuckDB connector (local: .duckdb file, or a directory/file of Parquet/
//! CSV/TSV). Always read-only. Single-writer lock conflicts surface as an
//! actionable error ("close the other process or use a copy").

use super::guard::assert_single_select;
use super::types::*;
use super::DataSource;
use async_trait::async_trait;
use querora_contracts::{ColumnMeta, ErrorCode, ToolError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Connection to a local DuckDB / Parquet / CSV source.
pub struct DuckDbSource {
    conn: Arc<Mutex<duckdb::Connection>>,
    kind: DuckKind,
}

enum DuckKind {
    Database,
    FileList(Vec<(String, PathBuf)>), // (table alias, path)
}

fn param_str<'a>(params: &'a serde_json::Value, key: &str) -> Result<&'a str, ToolError> {
    params.get(key).and_then(|v| v.as_str()).ok_or_else(|| {
        ToolError::new(
            ErrorCode::SourceUnavailable,
            format!("duckdb source requires `{key}` param"),
        )
    })
}

fn duck_type(t: &str) -> ColumnMeta {
    let t = t.to_lowercase();
    if t.contains("int") {
        ColumnMeta::Integer
    } else if t.contains("double") || t.contains("float") || t.contains("decimal") {
        ColumnMeta::Number
    } else if t.contains("bool") {
        ColumnMeta::Boolean
    } else if t.contains("date") || t.contains("time") {
        ColumnMeta::Temporal
    } else {
        ColumnMeta::String
    }
}

fn lock_error(path: &str) -> ToolError {
    ToolError::new(
        ErrorCode::SourceUnavailable,
        format!("DuckDB file is locked by another DuckDB process: {path}. Close the other process or use a copy of the file."),
    )
}

fn map_err(path: &str, e: duckdb::Error) -> ToolError {
    let msg = e.to_string();
    if msg.contains("lock")
        || msg.contains("Conflicting lock")
        || msg.contains("being used by another")
    {
        lock_error(path)
    } else {
        ToolError::new(ErrorCode::SourceUnavailable, format!("duckdb: {e}"))
    }
}

impl DuckDbSource {
    /// Open read-only. `path` may be a `.duckdb` database file, a single
    /// `.parquet`/`.csv`/`.tsv` file, or a directory of those.
    pub async fn connect(params: &serde_json::Value, _secret: &str) -> Result<Self, ToolError> {
        let path = PathBuf::from(param_str(params, "path")?);
        let path_str = path.display().to_string();
        if !path.exists() {
            return Err(ToolError::new(
                ErrorCode::SourceUnavailable,
                format!("path not found: {path_str}"),
            ));
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_lowercase();
        let is_db = ext == "duckdb" || ext == "db";
        let files = if is_db {
            vec![]
        } else if path.is_dir() {
            let mut v = Vec::new();
            for e in std::fs::read_dir(&path)
                .map_err(|e| ToolError::new(ErrorCode::SourceUnavailable, e.to_string()))?
            {
                let p = e
                    .map_err(|e| ToolError::new(ErrorCode::SourceUnavailable, e.to_string()))?
                    .path();
                let x = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or_default()
                    .to_lowercase();
                if matches!(x.as_str(), "parquet" | "csv" | "tsv") {
                    let alias = p
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("data")
                        .to_string();
                    v.push((alias, p));
                }
            }
            v
        } else {
            let alias = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("data")
                .to_string();
            vec![(alias, path.clone())]
        };

        let kind = if is_db {
            DuckKind::Database
        } else {
            DuckKind::FileList(files)
        };

        // :memory: driver db (the catalog data lives in the target file)
        let conn = tokio::task::spawn_blocking(move || {
            duckdb::Connection::open_in_memory()
                .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))
        })
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))??;

        // attach the real database read-only when it is a .duckdb file
        if is_db {
            let p = path_str.clone();
            conn.execute_batch(&format!("ATTACH '{p}' AS qdb (READ_ONLY);"))
                .map_err(|e| map_err(&p, e))?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            kind,
        })
    }

    fn table_expr(&self, table: &str) -> Result<String, ToolError> {
        match &self.kind {
            DuckKind::Database => Ok(format!("qdb.\"{}\"", table.replace('"', "\"\""))),
            DuckKind::FileList(files) => files
                .iter()
                .find(|(alias, _)| alias == table)
                .map(|(_, p)| format!("'{}'", p.display().to_string().replace('\'', "''")))
                .ok_or_else(|| {
                    ToolError::new(ErrorCode::NotFound, format!("table `{table}` not found"))
                }),
        }
    }

    fn run<T>(
        &self,
        path_desc: &str,
        f: impl FnOnce(&duckdb::Connection) -> duckdb::Result<T>,
    ) -> Result<T, ToolError> {
        let conn = self.conn.lock().expect("duckdb conn poisoned");
        conn.query_row("SELECT 1", [], |_| Ok(())).ok(); // liveness probe
        f(&conn).map_err(|e| map_err(path_desc, e))
    }
}

fn value_of(v: duckdb::types::Value) -> serde_json::Value {
    use duckdb::types::Value as V;
    match v {
        V::Null => serde_json::Value::Null,
        V::Integer(i) => serde_json::Value::from(i),
        V::Real(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null),
        V::Text(s) => serde_json::Value::from(s),
        V::Timestamp(us, _) | V::TimestampMicros(us, _) => serde_json::Value::String(
            chrono::DateTime::from_timestamp_micros(us)
                .map(|d| d.to_string())
                .unwrap_or_default(),
        ),
        V::Date(d) => serde_json::Value::String(d.to_string()),
        other => serde_json::Value::String(format!("{other:?}")),
    }
}

#[async_trait]
impl DataSource for DuckDbSource {
    async fn catalog(&self) -> Result<DatabaseCatalog, ToolError> {
        let this = Self {
            conn: self.conn.clone(),
            kind: clone_kind(&self.kind),
        };
        tokio::task::spawn_blocking(move || {
            let mut tables = Vec::new();
            match &this.kind {
                DuckKind::Database => {
                    let rows: Vec<(String, String)> = this
                        .run("duckdb", |c| {
                            let mut stmt = c.prepare("SELECT table_name, table_type FROM duckdb_tables()")?;
                            let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
                            it.collect::<duckdb::Result<Vec<_>>>()
                        })
                        .map_err(|e| e)?;
                    for (name, _ty) in rows {
                        let cols = this.run("duckdb", |c| {
                            let mut stmt =
                                c.prepare(&format!("SELECT column_name, data_type, is_nullable FROM duckdb_columns() WHERE table_name = ?1 ORDER BY column_index"))?;
                            let it = stmt.query_map([name.as_str()], |r| {
                                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?))
                            })?;
                            it.collect::<duckdb::Result<Vec<_>>>()
                        })?;
                        tables.push(TableInfo {
                            name,
                            is_view: false,
                            columns: cols
                                .into_iter()
                                .map(|(n, t, nullable)| ColumnInfo { name: n, data_type: t, nullable, primary_key: false })
                                .collect(),
                            foreign_keys: vec![],
                        });
                    }
                }
                DuckKind::FileList(files) => {
                    for (alias, path) in files {
                        let expr = format!("'{}'", path.display().to_string().replace('\'', "''"));
                        let cols: Vec<(String, String)> = this
                            .run(&path.display().to_string(), |c| {
                                let mut stmt = c.prepare(&format!("DESCRIBE SELECT * FROM {expr}"))?;
                                let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
                                it.collect::<duckdb::Result<Vec<_>>>()
                            })
                            .map_err(|e| e)?;
                        tables.push(TableInfo {
                            name: alias.clone(),
                            is_view: false,
                            columns: cols
                                .into_iter()
                                .map(|(n, t)| ColumnInfo { name: n, data_type: t, nullable: true, primary_key: false })
                                .collect(),
                            foreign_keys: vec![],
                        });
                    }
                }
            }
            Ok(DatabaseCatalog { tables })
        })
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?
    }

    async fn profile(
        &self,
        table: &str,
        column: &str,
        sample: u64,
    ) -> Result<ColumnProfile, ToolError> {
        let expr = self.table_expr(table)?;
        let sql_stats = format!(
            "SELECT COUNT(*), COUNT(\"{column}\"), COUNT(DISTINCT \"{column}\"), MIN(\"{column}\"), MAX(\"{column}\") FROM (SELECT * FROM {expr} LIMIT {sample})"
        );
        let sql_top = format!(
            "SELECT \"{column}\", COUNT(*) c FROM (SELECT * FROM {expr} LIMIT {sample}) GROUP BY 1 ORDER BY c DESC, 1 LIMIT 10"
        );
        let this = Self {
            conn: self.conn.clone(),
            kind: clone_kind(&self.kind),
        };
        tokio::task::spawn_blocking(move || {
            let (n, non_null, distinct, min_v, max_v): (
                i64,
                i64,
                i64,
                Option<duckdb::types::Value>,
                Option<duckdb::types::Value>,
            ) = this.run(&expr, |c| {
                c.query_row(&sql_stats, [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
            })?;
            let mut top: Vec<(String, u64)> = Vec::new();
            if let Ok(mut stmt) = {
                let conn = this.conn.lock().expect("duckdb conn poisoned");
                conn.prepare(&sql_top)
            } {
                let it = stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, Option<duckdb::types::Value>>(0)?,
                        r.get::<_, i64>(1)?,
                    ))
                })?;
                for row in it.flatten() {
                    if let Some(v) = row.0 {
                        top.push((
                            value_of(v).to_string().trim_matches('"').to_string(),
                            row.1 as u64,
                        ));
                    }
                }
            }
            let mut p = ColumnProfile {
                distinct_count: Some(distinct as u64),
                null_ratio: if n > 0 {
                    Some(((n - non_null) as f64) / n as f64)
                } else {
                    Some(0.0)
                },
                min: min_v.map(value_of),
                max: max_v.map(value_of),
                top_values: top,
                time_range: None,
                sampled_rows: n as u64,
            };
            if matches!(p.min, Some(serde_json::Value::String(_))) && p.min == p.max.clone() {
                p.time_range = None; // same-value string; not temporal signal
            }
            Ok(p)
        })
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?
    }

    async fn execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        cap: RowCap,
    ) -> Result<RawRows, ToolError> {
        let sql = assert_single_select(sql)?;
        // substitute ? placeholders with quoted literals (duckdb crate lacks
        // dynamic binds on raw SQL; values were parameterized upstream and are
        // re-bound here as typed literals — the guard already vetted shape)
        let mut bound = sql;
        for p in params {
            let lit = match p {
                serde_json::Value::Null => "NULL".to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::String(s) => format!("'{}'", s.replace('\'', "''")),
                other => format!("'{}'", other.to_string().replace('\'', "''")),
            };
            bound = bound.replacen('?', &lit, 1);
        }
        let wrapped = format!("SELECT * FROM ({bound}) AS _q LIMIT {}", cap.limit + 1);
        let this = Self {
            conn: self.conn.clone(),
            kind: clone_kind(&self.kind),
        };
        tokio::task::spawn_blocking(move || {
            let conn = this.conn.lock().expect("duckdb conn poisoned");
            let mut stmt = conn.prepare(&wrapped).map_err(|e| {
                ToolError::new(ErrorCode::SourceUnavailable, format!("duckdb: {e}"))
            })?;
            let col_count = stmt.column_count();
            let columns: Vec<String> = (0..col_count)
                .map(|i| stmt.column_name(i).unwrap_or_default().to_string())
                .collect();
            let column_types: Vec<ColumnMeta> = (0..col_count)
                .map(|i| duck_type(stmt.column_type(i).to_string().as_str()))
                .collect();
            let mut rows_out = Vec::new();
            let it = stmt
                .query_map([], |r| {
                    let mut row = Vec::with_capacity(col_count);
                    for i in 0..col_count {
                        row.push(value_of(r.get::<_, duckdb::types::Value>(i)?));
                    }
                    Ok(row)
                })
                .map_err(|e| {
                    ToolError::new(ErrorCode::SourceUnavailable, format!("duckdb: {e}"))
                })?;
            for row in it {
                rows_out.push(row.map_err(|e| {
                    ToolError::new(ErrorCode::SourceUnavailable, format!("duckdb: {e}"))
                })?);
                if rows_out.len() > cap.limit as usize {
                    break;
                }
            }
            rows_out.truncate(cap.limit as usize);
            Ok(RawRows {
                columns,
                column_types,
                rows: rows_out,
                row_cap: cap.limit,
            })
        })
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?
    }

    fn dialect(&self) -> Dialect {
        Dialect::DuckDb
    }
}

fn clone_kind(k: &DuckKind) -> DuckKind {
    match k {
        DuckKind::Database => DuckKind::Database,
        DuckKind::FileList(v) => DuckKind::FileList(v.clone()),
    }
}

impl Clone for DuckKind {
    fn clone(&self) -> Self {
        clone_kind(self)
    }
}

impl DuckKind {
    /// Whether this is a database file.
    pub fn is_database(&self) -> bool {
        matches!(self, Self::Database)
    }
}

/// Path bookkeeping used in errors.
impl DuckDbSource {
    /// Human description of what this source points at.
    pub fn describe(&self) -> String {
        match &self.kind {
            DuckKind::Database => "duckdb database".into(),
            DuckKind::FileList(v) => format!("{} file(s)", v.len()),
        }
    }
}

/// Compile-time: `Path` import used in describe paths.
#[allow(dead_code)]
fn _path_marker(_: Path) {}
