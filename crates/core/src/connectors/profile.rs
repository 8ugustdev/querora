// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Sampled column profiling: LIMIT-based (never full scans), one standard
//! SQL shape per driver.

use super::types::ColumnProfile;
use querora_contracts::{ErrorCode, ToolError};
use sqlx::{Row, SqlitePool};

fn to_json_scalar(s: &str) -> serde_json::Value {
    if let Ok(i) = s.parse::<i64>() {
        serde_json::Value::from(i)
    } else if let Ok(f) = s.parse::<f64>() {
        serde_json::Value::from(f)
    } else {
        serde_json::Value::String(s.to_string())
    }
}

/// SQLite profiling.
pub async fn profile_sqlite(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    sample: u64,
) -> Result<ColumnProfile, ToolError> {
    let err = |e: sqlx::Error| {
        ToolError::new(ErrorCode::SourceUnavailable, format!("sqlite profile: {e}"))
    };
    let (n, non_null, distinct): (i64, i64, i64) = sqlx::query_as(&format!(
        "SELECT COUNT(*), COUNT(\"{column}\"), COUNT(DISTINCT \"{column}\")
         FROM (SELECT \"{column}\" FROM \"{table}\" LIMIT {sample})"
    ))
    .fetch_one(pool)
    .await
    .map_err(err)?;
    let (min_v, max_v): (Option<String>, Option<String>) = sqlx::query_as(&format!(
        "SELECT CAST(MIN(\"{column}\") AS TEXT), CAST(MAX(\"{column}\") AS TEXT)
         FROM (SELECT \"{column}\" FROM \"{table}\" LIMIT {sample})"
    ))
    .fetch_one(pool)
    .await
    .map_err(err)?;
    let top_rows = sqlx::query(&format!(
        "SELECT CAST(\"{column}\" AS TEXT), COUNT(*) FROM (SELECT \"{column}\" FROM \"{table}\" LIMIT {sample})
         GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT 10"
    ))
    .fetch_all(pool)
    .await
    .map_err(err)?;

    Ok(ColumnProfile {
        distinct_count: Some(distinct as u64),
        null_ratio: if n > 0 {
            Some(((n - non_null) as f64) / n as f64)
        } else {
            Some(0.0)
        },
        min: min_v.as_deref().map(to_json_scalar),
        max: max_v.as_deref().map(to_json_scalar),
        top_values: top_rows
            .iter()
            .filter_map(|r| {
                let v: Option<String> = r.try_get(0).ok().flatten();
                let c: i64 = r.try_get(1).unwrap_or(0);
                v.map(|v| (v, c as u64))
            })
            .collect(),
        time_range: None,
        sampled_rows: n as u64,
    })
}

/// Postgres profiling (text-cast results; driver-agnostic shape).
pub async fn profile_pg(
    pool: &sqlx::PgPool,
    table: &str,
    column: &str,
    sample: u64,
) -> Result<ColumnProfile, ToolError> {
    let err =
        |e: sqlx::Error| ToolError::new(ErrorCode::SourceUnavailable, format!("pg profile: {e}"));
    let (n, non_null, distinct): (i64, i64, i64) = sqlx::query_as(&format!(
        "SELECT COUNT(*), COUNT(\"{column}\"), COUNT(DISTINCT \"{column}\")
         FROM (SELECT \"{column}\" FROM \"{table}\" LIMIT {sample})"
    ))
    .fetch_one(pool)
    .await
    .map_err(err)?;
    let (min_v, max_v): (Option<String>, Option<String>) = sqlx::query_as(&format!(
        "SELECT CAST(MIN(\"{column}\") AS TEXT), CAST(MAX(\"{column}\") AS TEXT)
         FROM (SELECT \"{column}\" FROM \"{table}\" LIMIT {sample})"
    ))
    .fetch_one(pool)
    .await
    .map_err(err)?;
    let top_rows: Vec<(Option<String>, i64)> = sqlx::query_as(&format!(
        "SELECT CAST(\"{column}\" AS TEXT), COUNT(*) FROM (SELECT \"{column}\" FROM \"{table}\" LIMIT {sample})
         GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT 10"
    ))
    .fetch_all(pool)
    .await
    .map_err(err)?;

    Ok(ColumnProfile {
        distinct_count: Some(distinct as u64),
        null_ratio: if n > 0 {
            Some(((n - non_null) as f64) / n as f64)
        } else {
            Some(0.0)
        },
        min: min_v.as_deref().map(to_json_scalar),
        max: max_v.as_deref().map(to_json_scalar),
        top_values: top_rows
            .into_iter()
            .filter_map(|(v, c)| v.map(|v| (v, c as u64)))
            .collect(),
        time_range: None,
        sampled_rows: n as u64,
    })
}

/// MySQL profiling (8.x: CAST … AS CHAR).
pub async fn profile_mysql(
    pool: &sqlx::MySqlPool,
    table: &str,
    column: &str,
    sample: u64,
) -> Result<ColumnProfile, ToolError> {
    let err = |e: sqlx::Error| {
        ToolError::new(ErrorCode::SourceUnavailable, format!("mysql profile: {e}"))
    };
    let (n, non_null, distinct): (i64, i64, i64) = sqlx::query_as(&format!(
        "SELECT COUNT(*), COUNT(`{column}`), COUNT(DISTINCT `{column}`)
         FROM (SELECT `{column}` FROM `{table}` LIMIT {sample}) AS _s"
    ))
    .fetch_one(pool)
    .await
    .map_err(err)?;
    let (min_v, max_v): (Option<String>, Option<String>) = sqlx::query_as(&format!(
        "SELECT CAST(MIN(`{column}`) AS CHAR), CAST(MAX(`{column}`) AS CHAR)
         FROM (SELECT `{column}` FROM `{table}` LIMIT {sample}) AS _s"
    ))
    .fetch_one(pool)
    .await
    .map_err(err)?;
    let top_rows: Vec<(Option<String>, i64)> = sqlx::query_as(&format!(
        "SELECT CAST(`{column}` AS CHAR), COUNT(*) FROM (SELECT `{column}` FROM `{table}` LIMIT {sample}) AS _s
         GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT 10"
    ))
    .fetch_all(pool)
    .await
    .map_err(err)?;

    Ok(ColumnProfile {
        distinct_count: Some(distinct as u64),
        null_ratio: if n > 0 {
            Some(((n - non_null) as f64) / n as f64)
        } else {
            Some(0.0)
        },
        min: min_v.as_deref().map(to_json_scalar),
        max: max_v.as_deref().map(to_json_scalar),
        top_values: top_rows
            .into_iter()
            .filter_map(|(v, c)| v.map(|v| (v, c as u64)))
            .collect(),
        time_range: None,
        sampled_rows: n as u64,
    })
}
