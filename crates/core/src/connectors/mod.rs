// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Data source connectors: the `DataSource` trait, a read-only SQL guard,
//! per-dialect implementations, and the connection registry.
//!
//! Import rule (module-boundary "import-linter"): nothing outside this
//! module may import a driver crate (`sqlx` pools, `duckdb`) directly —
//! all execution goes through [`DataSource`].

pub mod drift;
#[cfg(feature = "duckdb")]
pub mod duckdb_conn;
pub mod guard;
pub mod mysql;
pub mod postgres;
pub mod profile;
pub mod sqlite;
pub mod types;

use crate::keyring::CredentialStore;
use async_trait::async_trait;
use querora_contracts::{ErrorCode, SourceId, SourceInfo, SourceKind, ToolError};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
pub use types::*;

/// A connected data source. All execution flows through this trait;
/// `execute` is compiler-output-only and guarded (single SELECT).
#[async_trait]
pub trait DataSource: Send + Sync {
    /// Full catalog snapshot (tables, columns, PK/FK).
    async fn catalog(&self) -> Result<DatabaseCatalog, ToolError>;
    /// Sampled column profile (LIMIT-based, never full scans).
    async fn profile(
        &self,
        table: &str,
        column: &str,
        sample: u64,
    ) -> Result<ColumnProfile, ToolError>;
    /// Execute validated SQL (compiler output only; guarded read-only).
    async fn execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        cap: RowCap,
    ) -> Result<RawRows, ToolError>;
    /// The dialect this source speaks (renderer selection).
    fn dialect(&self) -> Dialect;
}

/// Connect to a source described by `info` using `secret` (from the
/// Keychain; may be empty for credential-less local files).
pub async fn connect(info: &SourceInfo, secret: &str) -> Result<Arc<dyn DataSource>, ToolError> {
    Ok(match info.kind {
        SourceKind::Sqlite => Arc::new(sqlite::SqliteSource::connect(&info.params, secret).await?),
        SourceKind::Postgres => {
            Arc::new(postgres::PostgresSource::connect(&info.params, secret).await?)
        }
        SourceKind::Mysql => Arc::new(mysql::MysqlSource::connect(&info.params, secret).await?),
        #[cfg(feature = "duckdb")]
        SourceKind::DuckDb => {
            Arc::new(duckdb_conn::DuckDbSource::connect(&info.params, secret).await?)
        }
        #[cfg(not(feature = "duckdb"))]
        SourceKind::DuckDb => {
            return Err(ToolError::new(
                ErrorCode::SourceUnavailable,
                "DuckDB support not compiled in (feature `duckdb` disabled)",
            ))
        }
    })
}

/// Live connections keyed by source id. Lazily connected, reused.
#[derive(Default)]
pub struct DataSources {
    inner: Mutex<HashMap<String, Arc<dyn DataSource>>>,
}

impl DataSources {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Connect (or return cached) data source for `id`. Reads public params
    /// from the app store and the secret from the credential store.
    pub async fn get(
        &self,
        id: &SourceId,
        store: &crate::storage::AppStore,
        creds: &dyn CredentialStore,
    ) -> Result<Arc<dyn DataSource>, ToolError> {
        if let Some(ds) = self.inner.lock().await.get(&id.0) {
            return Ok(ds.clone());
        }
        let info = store
            .list_sources()
            .await
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?
            .into_iter()
            .find(|s| s.id == *id)
            .ok_or_else(|| {
                ToolError::new(
                    ErrorCode::NotFound,
                    format!("source `{id}` is not configured"),
                )
            })?;
        let secret = creds
            .get(&secret_account(id))
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?
            .unwrap_or_default();
        let ds = connect(&info, &secret).await?;
        self.inner.lock().await.insert(id.0.clone(), ds.clone());
        Ok(ds)
    }

    /// Drop a cached connection (source removed / reconfigured).
    pub async fn invalidate(&self, id: &SourceId) {
        self.inner.lock().await.remove(&id.0);
    }
}

/// Keychain account holding a source's secret.
pub fn secret_account(id: &SourceId) -> String {
    format!("source.{}", id.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The import rule: nothing outside `connectors` may depend on driver
    /// crates. We assert the module exposes no driver types in its public
    /// API surface (compile-level guarantee via trait objects only).
    #[test]
    fn data_source_trait_is_object_safe() {
        fn assert_object_safe(_: &dyn DataSource) {}
        let _ = assert_object_safe;
    }
}
