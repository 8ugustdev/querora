// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! FTS5-backed semantic retrieval. Custom tokenizer table provides
//! trigram-ish normalization + synonymy via aliases (red-team #8:
//! "revenue" must match "Net Revenue MRR"), so `search_semantics` scales
//! to 500-table sources without full-catalog prompt dumps.

use crate::storage::AppStore;
use querora_contracts::{ErrorCode, ToolError};
use std::time::Instant;

const FTS_SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS semantic_fts USING fts5(
  kind, id, label, aliases, description,
  tokenize = "unicode61 remove_diacritics 2"
);
CREATE TABLE IF NOT EXISTS semantic_fts_meta (source_id TEXT PRIMARY KEY, populated_at TEXT NOT NULL);
"#;

/// Ensure FTS tables exist.
pub async fn ensure_fts(store: &AppStore) -> Result<(), ToolError> {
    sqlx::raw_sql(FTS_SCHEMA)
        .execute(store.pool())
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(())
}

/// (Re)build the index for a graph.
pub async fn index_graph(
    store: &AppStore,
    graph: &querora_contracts::SemanticGraph,
) -> Result<(), ToolError> {
    ensure_fts(store).await?;
    let pool = store.pool();
    sqlx::query("DELETE FROM semantic_fts WHERE id IN (SELECT id FROM semantic_fts)")
        .execute(pool)
        .await
        .ok(); // best-effort clear for this source (ids are global per app db; fine for M0 single-source)
    sqlx::query("DELETE FROM semantic_fts")
        .execute(pool)
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    sqlx::query(
        "INSERT OR REPLACE INTO semantic_fts_meta (source_id, populated_at) VALUES (?1, ?2)",
    )
    .bind(&graph.source.0)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await
    .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;

    for (id, e) in &graph.entities {
        insert_row(pool, "entity", id, &e.label, &[], e.description.as_deref()).await?;
    }
    for (id, m) in &graph.metrics {
        insert_row(
            pool,
            "metric",
            id,
            &m.label,
            &m.aliases,
            m.description.as_deref(),
        )
        .await?;
    }
    for (id, d) in &graph.dimensions {
        insert_row(
            pool,
            "dimension",
            id,
            &d.label,
            &d.aliases,
            d.description.as_deref(),
        )
        .await?;
    }
    // value-aware search: sample dimension values ("HAY", "Bordlamper", …)
    for (dim_id, values) in &graph.value_index {
        for v in values {
            insert_row(pool, "value", dim_id, v, &[], None).await?;
        }
    }
    Ok(())
}

async fn insert_row(
    pool: &sqlx::SqlitePool,
    kind: &str,
    id: &str,
    label: &str,
    aliases: &[String],
    description: Option<&str>,
) -> Result<(), ToolError> {
    sqlx::query("INSERT INTO semantic_fts (kind, id, label, aliases, description) VALUES (?1, ?2, ?3, ?4, ?5)")
        .bind(kind)
        .bind(id)
        .bind(label)
        .bind(aliases.join(" "))
        .bind(description.unwrap_or(""))
        .execute(pool)
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(())
}

/// One search hit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    /// entity | metric | dimension.
    pub kind: String,
    /// Graph id.
    pub id: String,
    /// Label.
    pub label: String,
    /// Rank (bm25).
    pub rank: f64,
}

/// FTS5 retrieval with alias- and prefix-matching. Terms are prefix-quoted
/// so "rev" matches "revenue"; multi-term queries AND.
pub async fn search(store: &AppStore, query: &str, k: usize) -> Result<Vec<SearchHit>, ToolError> {
    ensure_fts(store).await?;
    let t0 = Instant::now();
    let raw_terms: Vec<String> = query
        .split_whitespace()
        .filter(|t| !STOPWORDS.contains(&t.to_lowercase().as_str()))
        .map(|t| {
            t.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|t| !t.is_empty())
        .collect();
    let terms: Vec<String> = raw_terms.iter().map(|t| format!("\"{t}\"*")).collect();
    // curated-alias boost: exact alias/label matches on metrics/dimensions
    // are fetched explicitly so bm25 junk (e.g. *_sum_cc_exp_year) can't
    // drown "margin"/"revenue" on conversational questions
    let mut alias_boost: Vec<SearchHit> = Vec::new();
    {
        // term set incl. plural-stripped variants, longest (most specific)
        // first so generic tokens ("last", "year") can't crowd out
        // "margin" within the per-pattern limit
        let mut pats: Vec<String> = Vec::new();
        for t in &raw_terms {
            let mut t2 = t.clone();
            for _ in 0..2 {
                pats.push(t2.clone());
                if t2.ends_with('s') {
                    t2.pop();
                } else {
                    break;
                }
            }
        }
        pats.sort_by_key(|p| std::cmp::Reverse(p.len()));
        // token-exact matching (NOT substring: "how" must not hit
        // "Is Show …"). Two passes — curated aliases first, labels
        // second — so an alias hit ("margin" → Margin %) outranks
        // label hits on junk columns ("compared" → Report Compared
        // Product Index). Per-pattern caps keep generic tokens from
        // crowding rarer ones.
        for (col, cap) in [("aliases", 3i64), ("label", 2i64)] {
            for pat in pats.iter().take(5) {
                // token-exact match on alias/label columns (NOT substring:
                // "how" must not hit "Is Show …"); combined with the FTS
                // rank index this stays a seek, not a scan
                let rows: Vec<(String, String, String)> = sqlx::query_as(
                    "SELECT kind, id, label FROM semantic_fts WHERE semantic_fts MATCH ?1 AND kind IN ('metric','dimension') LIMIT ?2",
                )
                .bind(format!("{col}:\"{pat}\""))
                // small per-pattern cap: generic tokens ("compared", "year")
                // must not crowd rarer terms out of the merged boost list
                .bind(cap)
                .fetch_all(store.pool())
                .await
                .unwrap_or_default();
                for (kind, id, label) in rows {
                    if !alias_boost.iter().any(|h| h.id == id) {
                        alias_boost.push(SearchHit {
                            kind,
                            id,
                            label,
                            rank: -100.0,
                        });
                    }
                }
            }
        }
    }

    let mut hits: Vec<SearchHit> = if terms.is_empty() {
        Vec::new()
    } else {
        let q = terms.join(" ");
        let rows: Vec<(String, String, String, f64)> = sqlx::query_as(
            "SELECT kind, id, label, rank FROM semantic_fts WHERE semantic_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        )
        .bind(q)
        .bind(k as i64)
        .fetch_all(store.pool())
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, format!("fts: {e}")))?;
        rows.into_iter()
            .map(|(kind, id, label, rank)| SearchHit {
                kind,
                id,
                label,
                rank,
            })
            .collect()
    };
    // substring fallback for VALUE rows (prefix FTS misses "lamp(s)" →
    // "Bordlamper"); plural-stripped bounded LIKE scans
    if !raw_terms.is_empty() && hits.iter().filter(|h| h.kind == "value").count() < k {
        let mut pats: Vec<String> = Vec::new();
        for t in &raw_terms {
            let mut t = t.clone();
            for _ in 0..2 {
                pats.push(format!("%{t}%"));
                if t.ends_with('s') {
                    t.pop();
                } else if t.ends_with("es") {
                    t.truncate(t.len() - 2);
                } else {
                    break;
                }
            }
        }
        let mut extra: Vec<(String, String)> = Vec::new();
        for pat in &pats {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT DISTINCT id, label FROM semantic_fts WHERE kind = 'value' AND label LIKE ?1 LIMIT ?2",
            )
            .bind(pat)
            .bind(k as i64)
            .fetch_all(store.pool())
            .await
            .unwrap_or_default();
            extra.extend(rows);
            if extra.len() >= k {
                break;
            }
        }
        let known: std::collections::BTreeSet<String> = hits
            .iter()
            .filter(|h| h.kind == "value")
            .map(|h| h.id.clone())
            .collect();
        for (id, label) in extra.into_iter().take(k) {
            if !known.contains(&id) {
                hits.push(SearchHit {
                    kind: "value".into(),
                    id,
                    label,
                    rank: 0.0,
                });
            }
        }
    }
    // merge alias-boosted hits first (boost wins ties), dedupe by kind+id
    if !alias_boost.is_empty() {
        let mut merged: Vec<SearchHit> = Vec::new();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for h in alias_boost.into_iter().chain(hits) {
            if seen.insert(format!("{}::{}", h.kind, h.id)) {
                merged.push(h);
            }
        }
        merged.truncate(k);
        tracing::debug!(
            "fts search {query:?} → {} hits (alias-boosted) in {:?}",
            merged.len(),
            t0.elapsed()
        );
        return Ok(merged);
    }
    hits.truncate(k);
    tracing::debug!(
        "fts search {query:?} → {} hits in {:?}",
        hits.len(),
        t0.elapsed()
    );
    Ok(hits)
}

/// Measure p95-ish latency (benchmark hook for the 500-table fixture).
pub async fn search_p95_ms(store: &AppStore, queries: &[&str], k: usize) -> Result<f64, ToolError> {
    let mut times: Vec<u128> = Vec::new();
    for q in queries {
        let t0 = Instant::now();
        search(store, q, k).await?;
        times.push(t0.elapsed().as_micros());
    }
    times.sort_unstable();
    Ok(times[times.len().saturating_sub(1).min(times.len() - 1)] as f64 / 1000.0)
}

const STOPWORDS: &[&str] = &[
    "by", "per", "the", "a", "an", "of", "for", "in", "on", "to", "and", "show", "me", "what",
    "which", "is", "are",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::shop_graph;
    use crate::storage::AppStore;

    #[tokio::test]
    async fn alias_synonymy_rev_matches_net_revenue_mrr() {
        let store = AppStore::open_in_memory().await.unwrap();
        index_graph(&store, &shop_graph()).await.unwrap();
        let hits = search(&store, "revenue", 10).await.unwrap();
        assert!(hits.iter().any(|h| h.id == "revenue"), "hits: {hits:?}");
        // and the alias phrase itself
        let hits = search(&store, "net revenue mrr", 10).await.unwrap();
        assert!(hits.iter().any(|h| h.id == "revenue"));
    }

    #[tokio::test]
    async fn prefix_match_and_multi_term() {
        let store = AppStore::open_in_memory().await.unwrap();
        index_graph(&store, &shop_graph()).await.unwrap();
        // multi-term is AND within a row: label+alias must both hit
        let hits = search(&store, "rev mrr", 10).await.unwrap();
        assert!(
            hits.iter().any(|h| h.id == "revenue"),
            "AND query: {hits:?}"
        );
        // multi-term AND with no single matching row: empty
        let hits = search(&store, "rev mon", 10).await.unwrap();
        assert!(hits.is_empty(), "no AND row: {hits:?}");
        // token-exact alias boost: conversational "turnover" (alias token)
        // surfaces revenue even though no row matches all terms
        let hits = search(&store, "how was sales overall", 10).await.unwrap();
        assert!(
            hits.first().map(|h| h.id.as_str()) == Some("revenue"),
            "exact-token boost first: {hits:?}"
        );
    }

    #[tokio::test]
    async fn p95_under_50ms_on_fixture() {
        let store = AppStore::open_in_memory().await.unwrap();
        index_graph(&store, &shop_graph()).await.unwrap();
        let p95 = search_p95_ms(
            &store,
            &[
                "revenue by month",
                "order status paid",
                "customer country",
                "aov",
                "plan",
            ],
            20,
        )
        .await
        .unwrap();
        assert!(p95 < 50.0, "p95 {p95}ms must be < 50ms");
    }
}
