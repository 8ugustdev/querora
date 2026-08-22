// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Conversation persistence: maps app chat sessions to driver-native
//! session ids, records agent versions, and surfaces `context_lost` when a
//! resume can no longer be honored.

use crate::storage::AppStore;
use querora_contracts::{AgentEvent, ErrorCode, ToolError};
use std::sync::Arc;

/// Persist the driver session handle for later resume.
pub async fn record_session(
    store: &Arc<AppStore>,
    session_id: &str,
    agent: &str,
    agent_session_id: Option<&str>,
    agent_version: &str,
) -> Result<(), ToolError> {
    store
        .set_session_agent(session_id, agent_session_id, Some(agent_version))
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    let _ = agent;
    Ok(())
}

/// Load the driver session handle for resume. `None` when unknown —
/// callers then start a fresh context and let the UI know.
pub async fn load_session(
    store: &Arc<AppStore>,
    session_id: &str,
) -> Result<Option<(String, Option<String>, Option<String>)>, ToolError> {
    let row = store
        .session(session_id)
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(row.map(|r| (r.agent, r.agent_session_id, r.agent_version)))
}

/// Append a user/agent exchange to the app chat history.
pub async fn append_exchange(
    store: &Arc<AppStore>,
    session_id: &str,
    prompt: &str,
    events: &[AgentEvent],
) -> Result<(), ToolError> {
    let answer = events
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::Answer { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    store
        .append_message(session_id, "user", &serde_json::json!({ "text": prompt }))
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    let tool_timeline: Vec<serde_json::Value> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { tool, args } => {
                Some(serde_json::json!({ "kind": "call", "tool": tool, "args": args }))
            }
            AgentEvent::ToolResult { tool, ok, summary, result_id } => Some(serde_json::json!({
                "kind": "result", "tool": tool, "ok": ok, "summary": summary, "result_id": result_id,
            })),
            AgentEvent::ContextLost { reason } => {
                Some(serde_json::json!({ "kind": "context_lost", "reason": reason }))
            }
            _ => None,
        })
        .collect();
    store
        .append_message(
            session_id,
            "agent",
            &serde_json::json!({ "text": answer, "tool_timeline": tool_timeline }),
        )
        .await
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::AppStore;
    use querora_contracts::AgentEvent;

    #[tokio::test]
    async fn round_trip_session_and_exchange() {
        let store = Arc::new(AppStore::open_in_memory().await.unwrap());
        store.create_session("s1", "claude", "q").await.unwrap();
        record_session(&store, "s1", "claude", Some("claude-sid-7"), "2.1.233")
            .await
            .unwrap();
        let (agent, sid, ver) = load_session(&store, "s1").await.unwrap().unwrap();
        assert_eq!(agent, "claude");
        assert_eq!(sid.as_deref(), Some("claude-sid-7"));
        assert_eq!(ver.as_deref(), Some("2.1.233"));

        let events = vec![
            AgentEvent::ToolCall {
                tool: "mcp__querora__search_semantics".into(),
                args: serde_json::json!({"query":"revenue"}),
            },
            AgentEvent::ToolResult {
                tool: "search_semantics".into(),
                ok: true,
                summary: "3 hits".into(),
                result_id: None,
            },
            AgentEvent::Answer {
                text: "Revenue grew.".into(),
            },
            AgentEvent::Done,
        ];
        append_exchange(&store, "s1", "revenue?", &events)
            .await
            .unwrap();
        let msgs = store.messages("s1").await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].content["text"], "Revenue grew.");
        assert_eq!(
            msgs[1].content["tool_timeline"].as_array().unwrap().len(),
            2
        );
    }
}
