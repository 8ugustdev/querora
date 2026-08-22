// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! SQLite app storage: sources, semantic versions (draft → published,
//! immutable), chat sessions/messages, audit log. Runtime-checked queries
//! (no compile-time DATABASE_URL coupling); migrations embedded.

use querora_contracts::{SemanticGraph, SourceId, SourceInfo, SourceKind};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Row;
use std::path::Path;
use std::str::FromStr;

/// Open (and migrate) the app database at `path`.
pub async fn open_db(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true);
    // `:memory:` DBs are per-connection: pool size must be 1 there.
    let pool = if path.as_os_str() == ":memory:" {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?
    } else {
        SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?
    };
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// Typed façade over the app database.
pub struct AppStore {
    pool: SqlitePool,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Publication state of a semantic version row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticStatus {
    /// Editable draft.
    Draft,
    /// Immutable published version.
    Published,
}

impl SemanticStatus {
    /// String form used in the DB.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
        }
    }
}

/// One row of `semantic_versions`.
#[derive(Debug, Clone)]
pub struct SemanticVersionRow {
    /// Row id.
    pub id: i64,
    /// Owning source.
    pub source: SourceId,
    /// Version tag (empty for drafts).
    pub version: String,
    /// Draft or published.
    pub status: SemanticStatus,
    /// The graph itself.
    pub graph: SemanticGraph,
    /// Creation timestamp (RFC-3339).
    pub created_at: String,
    /// Publication timestamp when published.
    pub published_at: Option<String>,
}

/// One row of `chat_messages`.
#[derive(Debug, Clone)]
pub struct ChatMessageRow {
    /// Row id.
    pub id: i64,
    /// Owning session id.
    pub session_id: String,
    /// user | agent | system | tool.
    pub role: String,
    /// Arbitrary JSON payload.
    pub content: serde_json::Value,
    /// Creation timestamp.
    pub created_at: String,
}

/// One row of `chat_sessions`.
#[derive(Debug, Clone)]
pub struct ChatSessionRow {
    /// Session id.
    pub id: String,
    /// claude | codex | pi | byok.
    pub agent: String,
    /// Driver-native session id (resume handle).
    pub agent_session_id: Option<String>,
    /// Driver version recorded at conversation time.
    pub agent_version: Option<String>,
    /// Human title.
    pub title: String,
    /// Creation timestamp.
    pub created_at: String,
    /// Last update timestamp.
    pub updated_at: String,
}

impl AppStore {
    /// Direct pool access (FTS5 + internal queries).
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Open the app store, running migrations.
    pub async fn open(path: &Path) -> Result<Self, sqlx::Error> {
        Ok(Self {
            pool: open_db(path).await?,
        })
    }

    /// Open an in-memory store (tests).
    pub async fn open_in_memory() -> Result<Self, sqlx::Error> {
        Ok(Self {
            pool: open_db(Path::new(":memory:")).await?,
        })
    }

    // ---- sources ----

    /// Insert or update a source (secrets go to the Keychain, never here).
    pub async fn upsert_source(&self, info: &SourceInfo) -> Result<(), sqlx::Error> {
        let ts = now();
        sqlx::query(
            "INSERT INTO sources (id, name, kind, params_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(id) DO UPDATE SET name = ?2, kind = ?3, params_json = ?4, updated_at = ?5",
        )
        .bind(&info.id.0)
        .bind(&info.name)
        .bind(match info.kind {
            SourceKind::Postgres => "postgres",
            SourceKind::Mysql => "mysql",
            SourceKind::Sqlite => "sqlite",
            SourceKind::DuckDb => "duckdb",
        })
        .bind(info.params.to_string())
        .bind(&ts)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// List all sources.
    pub async fn list_sources(&self) -> Result<Vec<SourceInfo>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, name, kind, params_json, created_at FROM sources ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| SourceInfo {
                id: SourceId::new(r.try_get::<String, _>("id").unwrap_or_default()),
                name: r.try_get("name").unwrap_or_default(),
                kind: match r.try_get::<String, _>("kind").unwrap_or_default().as_str() {
                    "postgres" => SourceKind::Postgres,
                    "mysql" => SourceKind::Mysql,
                    "duckdb" => SourceKind::DuckDb,
                    _ => SourceKind::Sqlite,
                },
                params: serde_json::from_str(
                    r.try_get::<String, _>("params_json")
                        .unwrap_or_else(|_| "{}".into())
                        .as_str(),
                )
                .unwrap_or(serde_json::json!({})),
                created_at: r.try_get("created_at").unwrap_or_default(),
            })
            .collect())
    }

    /// Delete a source (cascades semantic versions).
    pub async fn delete_source(&self, id: &SourceId) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM sources WHERE id = ?1")
            .bind(&id.0)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- semantic versions ----

    /// Save a draft graph (upserts latest draft for the source).
    pub async fn save_draft(
        &self,
        source: &SourceId,
        graph: &SemanticGraph,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO semantic_versions (source_id, version, status, graph_json, created_at)
             VALUES (?1, '', 'draft', ?2, ?3)",
        )
        .bind(&source.0)
        .bind(serde_json::to_string(graph).unwrap_or_default())
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Publish a draft immutably: stamps a version id and flips status.
    /// Returns the version tag.
    pub async fn publish(
        &self,
        draft_row_id: i64,
        graph: &SemanticGraph,
    ) -> Result<String, sqlx::Error> {
        let version = format!("v{}", chrono::Utc::now().format("%Y%m%d%H%M%S%3f"));
        let mut graph = graph.clone();
        graph.version = version.clone();
        graph.published = true;
        sqlx::query(
            "UPDATE semantic_versions
             SET status = 'published', version = ?1, graph_json = ?2, published_at = ?3
             WHERE id = ?4 AND status = 'draft'",
        )
        .bind(&version)
        .bind(serde_json::to_string(&graph).unwrap_or_default())
        .bind(now())
        .bind(draft_row_id)
        .execute(&self.pool)
        .await?;
        Ok(version)
    }

    /// Latest published graph for a source (the ONLY one the compiler accepts).
    pub async fn published_graph(
        &self,
        source: &SourceId,
    ) -> Result<Option<SemanticGraph>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT graph_json FROM semantic_versions
             WHERE source_id = ?1 AND status = 'published'
             ORDER BY published_at DESC LIMIT 1",
        )
        .bind(&source.0)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let raw: String = r.try_get("graph_json").unwrap_or_default();
            serde_json::from_str(&raw).map_err(|e| sqlx::Error::Decode(Box::new(e)))
        })
        .transpose()
    }

    /// Latest draft graph for a source.
    pub async fn latest_draft(
        &self,
        source: &SourceId,
    ) -> Result<Option<SemanticVersionRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, source_id, version, status, graph_json, created_at, published_at FROM semantic_versions
             WHERE source_id = ?1 AND status = 'draft' ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&source.0)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| semantic_row(&r)).transpose()
    }

    // ---- chat sessions ----

    /// Create a session.
    pub async fn create_session(
        &self,
        id: &str,
        agent: &str,
        title: &str,
    ) -> Result<(), sqlx::Error> {
        let ts = now();
        sqlx::query(
            "INSERT INTO chat_sessions (id, agent, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
        )
        .bind(id)
        .bind(agent)
        .bind(title)
        .bind(&ts)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Create the session row if absent (idempotent per chat turn).
    pub async fn create_session_if_missing(
        &self,
        id: &str,
        agent: &str,
        title: &str,
    ) -> Result<(), sqlx::Error> {
        let ts = now();
        sqlx::query(
            "INSERT OR IGNORE INTO chat_sessions (id, agent, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
        )
        .bind(id)
        .bind(agent)
        .bind(title)
        .bind(&ts)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// List sessions, newest activity first.
    pub async fn list_sessions(&self) -> Result<Vec<ChatSessionRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, agent, agent_session_id, agent_version, title, created_at, updated_at
             FROM chat_sessions ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ChatSessionRow {
                    id: r.try_get("id")?,
                    agent: r.try_get("agent")?,
                    agent_session_id: r.try_get("agent_session_id")?,
                    agent_version: r.try_get("agent_version")?,
                    title: r.try_get("title")?,
                    created_at: r.try_get("created_at")?,
                    updated_at: r.try_get("updated_at")?,
                })
            })
            .collect()
    }

    /// Set the session title if still empty (first prompt, truncated).
    pub async fn set_session_title_if_empty(
        &self,
        id: &str,
        title: &str,
    ) -> Result<(), sqlx::Error> {
        let t: String = title.chars().take(48).collect();
        sqlx::query(
            "UPDATE chat_sessions SET title = ?2 WHERE id = ?1 AND (title = '' OR title IS NULL)",
        )
        .bind(id)
        .bind(&t)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record driver-native resume info on a session.
    pub async fn set_session_agent(
        &self,
        id: &str,
        agent_session_id: Option<&str>,
        agent_version: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE chat_sessions SET agent_session_id = ?2, agent_version = ?3, updated_at = ?4 WHERE id = ?1",
        )
        .bind(id)
        .bind(agent_session_id)
        .bind(agent_version)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Append a message to a session.
    pub async fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &serde_json::Value,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO chat_messages (session_id, role, content_json, created_at) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(session_id)
        .bind(role)
        .bind(content.to_string())
        .bind(now())
        .execute(&self.pool)
        .await?;
        sqlx::query("UPDATE chat_sessions SET updated_at = ?2 WHERE id = ?1")
            .bind(session_id)
            .bind(now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// List messages of a session, oldest first.
    pub async fn messages(&self, session_id: &str) -> Result<Vec<ChatMessageRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, session_id, role, content_json, created_at FROM chat_messages
             WHERE session_id = ?1 ORDER BY id ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ChatMessageRow {
                    id: r.try_get("id")?,
                    session_id: r.try_get("session_id")?,
                    role: r.try_get("role")?,
                    content: serde_json::from_str(
                        r.try_get::<String, _>("content_json")
                            .unwrap_or_default()
                            .as_str(),
                    )
                    .unwrap_or(serde_json::Value::Null),
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Fetch a session row.
    pub async fn session(&self, id: &str) -> Result<Option<ChatSessionRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, agent, agent_session_id, agent_version, title, created_at, updated_at
             FROM chat_sessions WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(ChatSessionRow {
                id: r.try_get("id")?,
                agent: r.try_get("agent")?,
                agent_session_id: r.try_get("agent_session_id")?,
                agent_version: r.try_get("agent_version")?,
                title: r.try_get("title")?,
                created_at: r.try_get("created_at")?,
                updated_at: r.try_get("updated_at")?,
            })
        })
        .transpose()
    }

    // ---- catalog cache ----

    /// Cache (or refresh) the introspected catalog for a source.
    pub async fn set_catalog(
        &self,
        source: &SourceId,
        catalog: &crate::connectors::types::DatabaseCatalog,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO catalog_cache (source_id, catalog_json, cached_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(source_id) DO UPDATE SET catalog_json = ?2, cached_at = ?3",
        )
        .bind(&source.0)
        .bind(serde_json::to_string(catalog).unwrap_or_default())
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Last cached catalog for a source (if any).
    pub async fn cached_catalog(
        &self,
        source: &SourceId,
    ) -> Result<Option<crate::connectors::types::DatabaseCatalog>, sqlx::Error> {
        let row = sqlx::query("SELECT catalog_json FROM catalog_cache WHERE source_id = ?1")
            .bind(&source.0)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| {
            let raw: String = r.try_get("catalog_json").unwrap_or_default();
            serde_json::from_str(&raw).map_err(|e| sqlx::Error::Decode(Box::new(e)))
        })
        .transpose()
    }

    // ---- app settings (kv) ----

    /// Get a setting value (JSON string) by key.
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, sqlx::Error> {
        let row = sqlx::query("SELECT value FROM app_settings WHERE key = ?1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| r.try_get::<String, _>("value").ok()))
    }

    /// Set (upsert) a setting value (JSON string).
    pub async fn set_setting(&self, key: &str, value: &str) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO app_settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2")
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- audit ----

    /// Append an audit entry.
    pub async fn audit(&self, actor: &str, tool: &str, summary: &str) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO audit_log (ts, actor, tool, summary) VALUES (?1, ?2, ?3, ?4)")
            .bind(now())
            .bind(actor)
            .bind(tool)
            .bind(summary)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Recent audit entries (newest first), for tests and the UI.
    pub async fn audit_entries(
        &self,
        limit: u32,
    ) -> Result<Vec<(String, String, String, String)>, sqlx::Error> {
        let rows =
            sqlx::query("SELECT ts, actor, tool, summary FROM audit_log ORDER BY id DESC LIMIT ?1")
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.try_get::<String, _>("ts").unwrap_or_default(),
                    r.try_get::<String, _>("actor").unwrap_or_default(),
                    r.try_get::<String, _>("tool").unwrap_or_default(),
                    r.try_get::<String, _>("summary").unwrap_or_default(),
                )
            })
            .collect())
    }
}

fn semantic_row(r: &sqlx::sqlite::SqliteRow) -> Result<SemanticVersionRow, sqlx::Error> {
    let raw: String = r.try_get("graph_json")?;
    let graph = serde_json::from_str(&raw).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    let status = match r.try_get::<String, _>("status")?.as_str() {
        "published" => SemanticStatus::Published,
        _ => SemanticStatus::Draft,
    };
    Ok(SemanticVersionRow {
        id: r.try_get("id")?,
        source: SourceId::new(r.try_get::<String, _>("source_id")?),
        version: r.try_get("version")?,
        status,
        graph,
        created_at: r.try_get("created_at")?,
        published_at: r.try_get("published_at")?,
    })
}

/// Sanity: chrono parse helper used by UI-facing code later.
pub fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::from_str(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use querora_contracts::{AggOp, Entity, Metric, MetricExpr};
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn migrations_apply_and_source_crud_works() {
        let store = AppStore::open_in_memory().await.unwrap();
        let info = SourceInfo {
            id: SourceId::new("shop"),
            name: "Shop".into(),
            kind: SourceKind::Sqlite,
            params: serde_json::json!({ "path": "/tmp/shop.db" }),
            created_at: now(),
        };
        store.upsert_source(&info).await.unwrap();
        store.upsert_source(&info).await.unwrap(); // idempotent upsert
        let sources = store.list_sources().await.unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id.0, "shop");
        assert_eq!(sources[0].params["path"], "/tmp/shop.db");

        store.delete_source(&SourceId::new("shop")).await.unwrap();
        assert!(store.list_sources().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn draft_publish_and_published_graph() {
        let store = AppStore::open_in_memory().await.unwrap();
        store
            .upsert_source(&SourceInfo {
                id: SourceId::new("shop"),
                name: "Shop".into(),
                kind: SourceKind::Sqlite,
                params: serde_json::json!({}),
                created_at: now(),
            })
            .await
            .unwrap();

        let mut graph = SemanticGraph {
            source: SourceId::new("shop"),
            version: String::new(),
            published: false,
            entities: BTreeMap::from([(
                "orders".to_string(),
                Entity {
                    id: "orders".into(),
                    label: "Orders".into(),
                    table: "orders".into(),
                    description: None,
                    definition_sql: None,
                },
            )]),
            metrics: BTreeMap::from([(
                "revenue".to_string(),
                Metric {
                    id: "revenue".into(),
                    label: "Revenue".into(),
                    entity_id: "orders".into(),
                    expr: MetricExpr {
                        op: AggOp::Sum,
                        column: Some("amount_total".into()),
                        human_formula: None,
                        combination: None,
                    },
                    aliases: vec![],
                    description: None,
                },
            )]),
            dimensions: BTreeMap::new(),
            relationships: vec![],
            value_index: Default::default(),
        };
        store
            .save_draft(&SourceId::new("shop"), &graph)
            .await
            .unwrap();
        let draft = store
            .latest_draft(&SourceId::new("shop"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(draft.status, SemanticStatus::Draft);

        let version = store.publish(draft.id, &graph).await.unwrap();
        assert!(version.starts_with('v'));
        graph.version = version.clone();

        let published = store
            .published_graph(&SourceId::new("shop"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(published.version, version);
        assert!(published.published);
    }

    #[tokio::test]
    async fn chat_sessions_and_messages() {
        let store = AppStore::open_in_memory().await.unwrap();
        store
            .create_session("s1", "claude", "revenue q")
            .await
            .unwrap();
        store
            .set_session_agent("s1", Some("claude-sid-42"), Some("2.1.233"))
            .await
            .unwrap();
        store
            .append_message(
                "s1",
                "user",
                &serde_json::json!({ "text": "monthly revenue?" }),
            )
            .await
            .unwrap();
        let msgs = store.messages("s1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        let sess = store.session("s1").await.unwrap().unwrap();
        assert_eq!(sess.agent_session_id.as_deref(), Some("claude-sid-42"));

        store
            .audit("toolapi", "search_semantics", "q=revenue")
            .await
            .unwrap();
    }

    /// Plan requirement: SQLite contains zero secrets. The schema has no
    /// credential column at all; assert the sources table shape.
    #[tokio::test]
    async fn sources_table_has_no_secret_columns() {
        let store = AppStore::open_in_memory().await.unwrap();
        let row = sqlx::query("PRAGMA table_info(sources)")
            .fetch_all(&store.pool)
            .await
            .unwrap();
        let cols: Vec<String> = row
            .iter()
            .map(|r| r.try_get::<String, _>("name").unwrap())
            .collect();
        for forbidden in ["secret", "password", "token", "credential"] {
            assert!(
                !cols.iter().any(|c| c.to_lowercase().contains(forbidden)),
                "sources table must not contain `{forbidden}` columns: {cols:?}"
            );
        }
    }
}
