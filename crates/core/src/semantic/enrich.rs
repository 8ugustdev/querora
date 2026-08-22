// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Agent enrichment: optional AI pass over a heuristic draft — business
//! names/descriptions, metric suggestions, aliases. The agent receives an
//! FTS-retrieved CATALOG SUBSET (never the full catalog) and returns a
//! `SemanticGraph`; schema-validated, else rejected → heuristic fallback
//! (pre-decided risk mitigation). Human reviews the diff before publish.

use super::retrieval;
use crate::agents::{claude::ClaudeDriver, AgentDriver, RunRequest};
use crate::storage::AppStore;
use querora_contracts::semantic::SemanticGraph;
use querora_contracts::{ErrorCode, ToolError};
use std::path::PathBuf;

/// Enrichment outcome.
#[derive(Debug)]
pub enum EnrichResult {
    /// Agent returned a valid draft graph (review before publish!).
    Enriched(SemanticGraph),
    /// Agent unavailable/invalid output — heuristic draft stands.
    FellBack(String),
}

/// Prompt the connected agent to enrich `draft` given retrieval context.
/// BOUNDED: the payload carries only the FTS-retrieved subset (≤20 items
/// with full detail) plus a compact global id list — never the full graph
/// (context-explosion guard for 500-table sources). The agent returns a
/// JSON patch array; we apply it locally, so structure cannot drift.
pub async fn enrich(
    store: &AppStore,
    draft: &SemanticGraph,
    run_dir: PathBuf,
    socket: PathBuf,
    token: &str,
) -> Result<EnrichResult, ToolError> {
    // auto-hint from the draft's own labels; retrieval bounds the payload
    let hint = draft
        .metrics
        .values()
        .take(5)
        .map(|m| m.label.clone())
        .chain(draft.entities.values().take(5).map(|e| e.label.clone()))
        .collect::<Vec<_>>()
        .join(", ");
    let hits = retrieval::search(store, &hint, 20)
        .await
        .unwrap_or_default();

    // detailed entries ONLY for retrieved ids
    let mut detail: Vec<serde_json::Value> = Vec::new();
    for h in &hits {
        let item = match h.kind.as_str() {
            "metric" => draft.metrics.get(&h.id).map(|m| {
                serde_json::json!({ "kind": "metric", "id": m.id, "label": m.label, "aliases": m.aliases })
            }),
            "dimension" => draft.dimensions.get(&h.id).map(|d| {
                serde_json::json!({ "kind": "dimension", "id": d.id, "label": d.label, "aliases": d.aliases })
            }),
            "entity" => draft.entities.get(&h.id).map(|e| {
                serde_json::json!({ "kind": "entity", "id": e.id, "label": e.label })
            }),
            _ => None,
        };
        if let Some(item) = item {
            detail.push(item);
        }
    }
    // compact global id census (ids only, no structure)
    let census = serde_json::json!({
        "entities": draft.entities.keys().collect::<Vec<_>>(),
        "metrics": draft.metrics.keys().collect::<Vec<_>>(),
        "dimensions": draft.dimensions.keys().collect::<Vec<_>>(),
    });

    let prompt = format!(
        "You are improving a BI semantic layer. Below are (a) sampled items from the draft and (b) the full id census. Return ONLY a JSON array of patches: [{{\"kind\":\"metric|dimension|entity\",\"id\":\"...\",\"label\":\"...\",\"description\":\"...\",\"aliases\":[\"...\"]}}]. Rules: ids MUST exist in the census and match their kind; you may ONLY set label/description/aliases; add no new ids; keep labels short and business-friendly; include synonyms users would type. 20-60 patches max.\n(a) sampled items:\n{}\n(b) id census:\n{}",
        serde_json::to_string_pretty(&detail).unwrap_or_default(),
        serde_json::to_string_pretty(&census).unwrap_or_default(),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let driver = ClaudeDriver;
    let outcome = driver
        .run(
            RunRequest {
                prompt,
                resume: None,
                socket,
                run_dir,
                token: token.to_string(),
            },
            tx,
        )
        .await;

    match outcome {
        Ok(_) => {
            let mut answer = String::new();
            while let Some(ev) = rx.recv().await {
                if let querora_contracts::AgentEvent::Answer { text } = ev {
                    answer = text;
                }
            }
            apply_patches(&answer, draft).map(EnrichResult::Enriched)
        }
        Err(e) => Ok(EnrichResult::FellBack(format!("agent unavailable: {e}"))),
    }
}

/// Extract the JSON patch array from the agent answer and apply it.
/// Structural safety by construction: only label/description/aliases can
/// change; unknown ids / wrong kinds / malformed JSON → structured error.
pub fn apply_patches(answer: &str, draft: &SemanticGraph) -> Result<SemanticGraph, ToolError> {
    let json = extract_json(answer).ok_or_else(|| {
        ToolError::new(
            ErrorCode::InvalidIr,
            "agent answer contained no JSON patches (fallback to heuristic)",
        )
    })?;
    let patches: Vec<serde_json::Value> = match json {
        serde_json::Value::Array(a) => a,
        _ => {
            return Err(ToolError::new(
                ErrorCode::InvalidIr,
                "expected a JSON array of patches",
            ))
        }
    };
    let mut g = draft.clone();
    let mut applied = 0usize;
    for p in patches {
        let kind = p["kind"].as_str().unwrap_or_default();
        let id = p["id"].as_str().unwrap_or_default().to_string();
        if id.is_empty() {
            continue;
        }
        let label = p["label"].as_str().map(str::to_string);
        let description = p["description"].as_str().map(str::to_string);
        let aliases: Option<Vec<String>> = p["aliases"].as_array().map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        });
        let target_ok = match kind {
            "metric" => g
                .metrics
                .get_mut(&id)
                .map(|m| {
                    if let Some(l) = label {
                        m.label = l;
                    }
                    m.description = description.or_else(|| m.description.clone());
                    if let Some(al) = aliases {
                        m.aliases = al;
                    }
                    true
                })
                .unwrap_or(false),
            "dimension" => g
                .dimensions
                .get_mut(&id)
                .map(|d| {
                    if let Some(l) = label {
                        d.label = l;
                    }
                    d.description = description.or_else(|| d.description.clone());
                    if let Some(al) = aliases {
                        d.aliases = al;
                    }
                    true
                })
                .unwrap_or(false),
            "entity" => g
                .entities
                .get_mut(&id)
                .map(|e| {
                    if let Some(l) = label {
                        e.label = l;
                    }
                    e.description = description.or_else(|| e.description.clone());
                    true
                })
                .unwrap_or(false),
            _ => false,
        };
        if target_ok {
            applied += 1;
        }
    }
    if applied == 0 {
        return Err(ToolError::new(
            ErrorCode::InvalidIr,
            "no patches applied (ids not found or kinds mismatched)",
        ));
    }
    tracing::info!("enrichment applied {applied} patches");
    Ok(g)
}

/// Extract + validate the JSON graph from the agent answer. Accepts a
/// fenced or bare JSON object; rejects anything structurally off.
pub fn parse_graph(answer: &str, draft: &SemanticGraph) -> Result<SemanticGraph, ToolError> {
    let json = extract_json(answer).ok_or_else(|| {
        ToolError::new(
            ErrorCode::InvalidIr,
            "agent answer contained no JSON graph (fallback to heuristic)",
        )
    })?;
    let g: SemanticGraph = serde_json::from_value(json).map_err(|e| {
        ToolError::new(
            ErrorCode::InvalidIr,
            format!("graph JSON invalid ({e}) — falling back"),
        )
    })?;

    // invariants: same source; no REMOVED ids (enrichment only adds/edits)
    if g.source != draft.source {
        return Err(ToolError::new(
            ErrorCode::InvalidIr,
            "enriched graph changed source",
        ));
    }
    for id in draft.metrics.keys() {
        if !g.metrics.contains_key(id) {
            return Err(ToolError::new(
                ErrorCode::InvalidIr,
                format!("enriched graph dropped metric `{id}`"),
            ));
        }
    }
    for id in draft.dimensions.keys() {
        if !g.dimensions.contains_key(id) {
            return Err(ToolError::new(
                ErrorCode::InvalidIr,
                format!("enriched graph dropped dimension `{id}`"),
            ));
        }
    }
    for id in draft.entities.keys() {
        if !g.entities.contains_key(id) {
            return Err(ToolError::new(
                ErrorCode::InvalidIr,
                format!("enriched graph dropped entity `{id}`"),
            ));
        }
    }
    // structural columns must be untouched
    for (id, d) in &draft.dimensions {
        let got = &g.dimensions[id];
        if got.column != d.column || got.entity_id != d.entity_id {
            return Err(ToolError::new(
                ErrorCode::InvalidIr,
                format!("enriched graph mutated dimension `{id}` structurally"),
            ));
        }
    }
    Ok(g)
}

fn extract_json(answer: &str) -> Option<serde_json::Value> {
    // fenced ```json … ``` first
    if let Some(start) = answer.find("```json") {
        let rest = &answer[start + 7..];
        if let Some(end) = rest.find("```") {
            if let Ok(v) = serde_json::from_str(rest[..end].trim()) {
                return Some(v);
            }
        }
    }
    // bare outermost object OR array
    if let (Some(start), Some(end)) = (answer.find('{'), answer.rfind('}')) {
        if let Ok(v) = serde_json::from_str(&answer[start..=end]) {
            return Some(v);
        }
    }
    let start = answer.find('[')?;
    let end = answer.rfind(']')?;
    serde_json::from_str(&answer[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::shop_graph;

    fn draft() -> SemanticGraph {
        let mut g = shop_graph();
        g.published = false;
        g
    }

    #[test]
    fn patches_apply_to_labels_only() {
        let d = draft();
        let answer = r#"```json
[{"kind":"metric","id":"revenue","label":"Order Revenue","description":"Paid orders","aliases":["sales","turnover"]}]
```"#;
        let g = apply_patches(answer, &d).unwrap();
        assert_eq!(g.metrics["revenue"].label, "Order Revenue");
        assert_eq!(
            g.metrics["revenue"].aliases,
            vec!["sales".to_string(), "turnover".to_string()]
        );
        // structure untouched by construction
        assert_eq!(
            g.metrics["revenue"].expr.column,
            d.metrics["revenue"].expr.column
        );
    }

    #[test]
    fn unknown_ids_and_wrong_kinds_are_ignored_or_rejected() {
        let d = draft();
        // one valid + one unknown id → 1 applied, ok
        let g = apply_patches(
            r#"[{"kind":"metric","id":"revenue","label":"R"},{"kind":"metric","id":"ghost","label":"X"}]"#,
            &d,
        )
        .unwrap();
        assert_eq!(g.metrics["revenue"].label, "R");
        // all-unknown → structured error → caller falls back
        let err = apply_patches(r#"[{"kind":"metric","id":"ghost","label":"X"}]"#, &d).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidIr);
        // wrong kind for an existing id → ignored → zero applied → error
        let err =
            apply_patches(r#"[{"kind":"dimension","id":"revenue","label":"X"}]"#, &d).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidIr);
    }

    #[test]
    fn garbage_answer_is_structured_fallback() {
        let d = draft();
        let err = apply_patches("I could not produce JSON, sorry.", &d).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidIr);
        assert!(err.message.contains("fallback"));
    }
}
