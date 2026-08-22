// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Claude Code driver: `claude -p --output-format stream-json --verbose`
//! with the Querora MCP shim scoped in and default tools disallowed.
//!
//! Flag set pinned + probed by the Phase 5 spike (docs/agent-flags.md):
//! - MCP tools auto-approve headless via `--allowedTools "mcp__querora__*"`
//! - least-privilege via `--disallowedTools` on the standard defaults
//! - resume via `--resume <session_id>`; failures → `context_lost`

use super::{kill_tree, AgentDriver, RunOutcome, RunRequest};
use async_trait::async_trait;
use querora_contracts::{AgentEvent, AgentStatus, ErrorCode, ToolError};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// Default tools disabled in driver mode (least privilege).
pub const DISALLOWED_DEFAULTS: &str = "Bash,Edit,Write,NotebookEdit,WebFetch,WebSearch,Agent,Glob,Grep,Read,TodoWrite,TaskCreate,TaskUpdate,TaskList,KillShell";

/// Driver over the `claude` binary.
pub struct ClaudeDriver;

/// Write the per-session mcp-config (0600) pointing at the querora shim.
pub fn write_mcp_config(req: &RunRequest, shim: &std::path::Path) -> Result<PathBuf, ToolError> {
    let path = req
        .run_dir
        .join(format!("mcp-claude-{}.json", std::process::id()));
    let cfg = serde_json::json!({
        "mcpServers": {
            "querora": {
                "command": shim.display().to_string(),
                "args": [],
                "env": {
                    "QUERORA_SOCK": req.socket.display().to_string(),
                    "QUERORA_TOKEN": req.token,
                }
            }
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap())
        .and_then(|_| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            }
            #[cfg(not(unix))]
            {
                Ok(())
            }
        })
        .map_err(|e| ToolError::new(ErrorCode::Internal, format!("mcp-config write: {e}")))?;
    Ok(path)
}

/// Parse one stream-json event line into normalized events (never panics).
pub fn parse_event_line(line: &str, tx: &mut Vec<AgentEvent>, state: &mut Option<String>) {
    let v: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return, // fuzz guarantee: garbage ignored
    };
    let ty = v["type"].as_str().unwrap_or_default();
    match ty {
        "assistant" => {
            if let Some(blocks) = v["message"]["content"].as_array() {
                for b in blocks {
                    match b["type"].as_str().unwrap_or_default() {
                        "text" => {
                            if let Some(t) = b["text"].as_str() {
                                if !t.is_empty() {
                                    tx.push(AgentEvent::Token {
                                        text: t.to_string(),
                                    });
                                }
                            }
                        }
                        "tool_use" => {
                            tx.push(AgentEvent::ToolCall {
                                tool: b["name"].as_str().unwrap_or_default().to_string(),
                                args: b["input"].clone(),
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        "user" => {
            if let Some(blocks) = v["message"]["content"].as_array() {
                for b in blocks {
                    if b["type"] == "tool_result" {
                        let ok = !b.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                        let summary = summarize_tool_result(&b["content"]);
                        let result_id = extract_result_id(&summary);
                        tx.push(AgentEvent::ToolResult {
                            tool: String::new(),
                            ok,
                            summary,
                            result_id,
                        });
                    }
                }
            }
        }
        "result" => {
            *state = v["session_id"].as_str().map(str::to_string);
            if v["is_error"].as_bool().unwrap_or(false) {
                tx.push(AgentEvent::Failed {
                    error: v["result"].as_str().unwrap_or_default().to_string(),
                });
            } else {
                tx.push(AgentEvent::Answer {
                    text: v["result"].as_str().unwrap_or_default().to_string(),
                });
            }
            tx.push(AgentEvent::Done);
        }
        "system" if v["subtype"].as_str() == Some("error") => {
            tx.push(AgentEvent::Failed {
                error: v["error"].as_str().unwrap_or("claude system error").into(),
            });
        }
        _ => {}
    }
}

/// Pull `result_id` out of an execute_query payload (first key of the
/// pretty-printed AgentResult JSON — survives the 200-char summary cut).
pub fn extract_result_id(summary: &str) -> Option<String> {
    let i = summary.find("\"result_id\"")?;
    let rest = &summary[i + 12..];
    let q1 = rest.find('"')?;
    let tail = &rest[q1 + 1..];
    let q2 = tail.find('"')?;
    let id = &tail[..q2];
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

fn summarize_tool_result(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|i| i["text"].as_str())
            .next()
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
        serde_json::Value::String(s) => s.chars().take(200).collect(),
        other => serde_json::to_string(other)
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
    }
}

#[async_trait]
impl AgentDriver for ClaudeDriver {
    fn id(&self) -> &'static str {
        "claude"
    }

    async fn probe(&self) -> AgentStatus {
        super::probe::probe_claude().await
    }

    async fn run(
        &self,
        req: RunRequest,
        tx: mpsc::Sender<AgentEvent>,
    ) -> Result<RunOutcome, ToolError> {
        let shim = super::mcp_shim_path().ok_or_else(|| {
            ToolError::new(
                ErrorCode::SourceUnavailable,
                "querora-mcp shim binary not found (build crates/mcp)",
            )
        })?;
        let mcp_config = write_mcp_config(&req, &shim)?;
        let version = super::probe::bin_version("claude")
            .await
            .unwrap_or_default();

        let mut cmd = Command::new("claude");
        cmd.args([
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--mcp-config",
        ])
        .arg(&mcp_config)
        .arg("--allowedTools")
        .arg("mcp__querora__*")
        .arg("--disallowedTools")
        .arg(DISALLOWED_DEFAULTS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        if let Some(sid) = &req.resume {
            cmd.arg("--resume").arg(sid);
        }
        // isolate from repo-level CLAUDE.md noise
        cmd.env("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "1");

        let mut child = cmd.spawn().map_err(|e| {
            ToolError::new(ErrorCode::SourceUnavailable, format!("claude spawn: {e}"))
        })?;

        // prompt via stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let mut p = req.prompt.clone();
            p.push('\n');
            stdin.write_all(p.as_bytes()).await.ok();
            stdin.shutdown().await.ok();
        }

        let _ = tx
            .send(AgentEvent::Started {
                agent: "claude".into(),
                session_id: req.resume.clone(),
            })
            .await;

        let stdout = child.stdout.take().expect("stdout piped");
        let mut reader = BufReader::new(stdout).lines();
        let mut outcome = RunOutcome {
            agent_version: version.clone(),
            ..Default::default()
        };
        while let Ok(Some(line)) = reader.next_line().await {
            let mut events = Vec::new();
            parse_event_line(&line, &mut events, &mut outcome.session_id);
            for ev in events {
                if matches!(ev, AgentEvent::Answer { .. }) {
                    outcome.answered = true;
                }
                if matches!(ev, AgentEvent::Done) {
                    // stream continues after result? claude emits result last; still drain
                }
                if tx.send(ev).await.is_err() {
                    break;
                }
            }
        }
        let status = child
            .wait()
            .await
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
        let _ = std::fs::remove_file(&mcp_config);

        if !outcome.answered {
            let reason = if req.resume.is_some() {
                AgentEvent::ContextLost {
                    reason: format!("claude exited without an answer (status {status}); the session may be gone (agent upgraded?) — restate your question"),
                }
            } else {
                AgentEvent::Failed {
                    error: format!("claude exited without an answer (status {status})"),
                }
            };
            let _ = tx.send(reason).await;
            let _ = tx.send(AgentEvent::Done).await;
        }
        kill_tree(&mut child).await;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_lines_never_panic() {
        for garbage in [
            "",
            "not json",
            "{",
            "null",
            "[]",
            "{\"type\":}",
            "\u{1f980}",
            "{\"type\":\"assistant\",\"message\":{\"content\":\"not-an-array\"}}",
            "{\"type\":\"result\",\"result\":123}",
        ] {
            let mut out = Vec::new();
            let mut sid = None;
            parse_event_line(garbage, &mut out, &mut sid);
        }
    }

    #[test]
    fn parses_tool_use_and_result() {
        let mut out = Vec::new();
        let mut sid = None;
        parse_event_line(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"thinking..."},{"type":"tool_use","name":"mcp__querora__search_semantics","input":{"query":"revenue"}}]}}"#,
            &mut out,
            &mut sid,
        );
        assert!(matches!(&out[0], AgentEvent::Token { text } if text == "thinking..."));
        assert!(
            matches!(&out[1], AgentEvent::ToolCall { tool, .. } if tool == "mcp__querora__search_semantics")
        );
        out.clear();
        parse_event_line(
            r#"{"type":"result","result":"the answer","session_id":"abc","is_error":false}"#,
            &mut out,
            &mut sid,
        );
        assert_eq!(sid.as_deref(), Some("abc"));
        assert!(matches!(&out[0], AgentEvent::Answer { text } if text == "the answer"));
        assert!(matches!(out[1], AgentEvent::Done));
    }
}
