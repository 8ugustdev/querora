// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Codex CLI driver: `codex exec --json` with the Querora MCP server
//! injected via `-c` config overrides against the user's own CODEX_HOME
//! (their model + auth stay intact).
//!
//! Spike-verified recipe (docs/agent-flags.md, codex 0.147):
//! - MCP server inline-table override + scalar
//!   `mcp_servers.querora.requires_approval=false`
//! - sandboxed headless mode AUTO-CANCELS MCP tool elicitations
//!   (`request_user_input is not supported in exec mode`) — the only
//!   working mode is `--sandbox danger-full-access`, which is CONSENT-GATED
//!   (`CodexDriver::unsandboxed`) per the plan's red-line rule
//! - `-c features.default_mode_request_user_input=false` silences the
//!   unsupported-elicitation path
//! - resume via `codex exec resume <thread_id>`

use super::{kill_tree, AgentDriver, RunOutcome, RunRequest};
use async_trait::async_trait;
use querora_contracts::{AgentEvent, AgentStatus, ErrorCode, ToolError};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// Driver over the `codex` binary.
pub struct CodexDriver {
    /// User consented to running codex unsandboxed (Querora tools only).
    /// Codex 0.147 cancels MCP tool calls in sandboxed headless mode, so
    /// unsandboxed is the only working configuration — it MUST be gated
    /// behind explicit, recorded user consent (red-line rule).
    pub unsandboxed: bool,
}

impl CodexDriver {
    /// Sandboxed (default) driver — MCP calls will be cancelled by codex;
    /// surfaces the consent-needed error to the UI instead of silently
    /// failing.
    pub fn new() -> Self {
        Self { unsandboxed: false }
    }

    /// Unsandboxed driver — requires recorded user consent.
    pub fn unsandboxed() -> Self {
        Self { unsandboxed: true }
    }
}

impl Default for CodexDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the `-c` MCP override TOML for the querora shim (inline table).
/// `requires_approval` is applied as a separate scalar override.
pub fn mcp_override_toml(shim: &std::path::Path, socket: &std::path::Path, token: &str) -> String {
    format!(
        "mcp_servers.querora={{command={}, args=[], env={{QUERORA_SOCK={}, QUERORA_TOKEN={}}}}}",
        toml_str(&shim.display().to_string()),
        toml_str(&socket.display().to_string()),
        toml_str(token),
    )
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Parse one codex JSONL event into normalized events (never panics on
/// malformed input — fuzz guarantee). `events` accumulates within one line;
/// answer synthesis happens in `run()` from the token stream.
pub fn parse_event_line(line: &str, events: &mut Vec<AgentEvent>, thread_id: &mut Option<String>) {
    let v: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return,
    };
    let ty = v["type"].as_str().unwrap_or_default();
    match ty {
        "thread.started" => {
            *thread_id = v["thread_id"].as_str().map(str::to_string);
        }
        "item.completed" => {
            let item = &v["item"];
            match item["type"].as_str().unwrap_or_default() {
                "agent_message" => {
                    if let Some(text) = item["text"].as_str() {
                        if !text.is_empty() {
                            events.push(AgentEvent::Token {
                                text: text.to_string(),
                            });
                        }
                    }
                }
                "mcp_tool_call" => {
                    let ok = item["status"].as_str() == Some("completed");
                    events.push(AgentEvent::ToolCall {
                        tool: format!(
                            "mcp__querora__{}",
                            item["tool"].as_str().unwrap_or_default()
                        ),
                        args: item["arguments"].clone(),
                    });
                    let summary = summarize(&item["output"]);
                    events.push(AgentEvent::ToolResult {
                        tool: item["tool"].as_str().unwrap_or_default().to_string(),
                        ok,
                        result_id: super::claude::extract_result_id(&summary),
                        summary,
                    });
                }
                _ => {}
            }
        }
        "turn.completed" => {
            events.push(AgentEvent::Done);
        }
        "turn.failed" => {
            events.push(AgentEvent::Failed {
                error: v["error"]["message"]
                    .as_str()
                    .unwrap_or("codex turn failed")
                    .into(),
            });
            events.push(AgentEvent::Done);
        }
        "error" => {
            // reconnect noise filtered; real errors surface via turn.failed
        }
        _ => {}
    }
}

fn summarize(output: &serde_json::Value) -> String {
    match output {
        serde_json::Value::String(s) => s.chars().take(200).collect(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|i| i["text"].as_str())
            .next()
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
        serde_json::Value::Null => String::new(),
        other => serde_json::to_string(other)
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
    }
}

#[async_trait]
impl AgentDriver for CodexDriver {
    fn id(&self) -> &'static str {
        "codex"
    }

    async fn probe(&self) -> AgentStatus {
        super::probe::probe_codex().await
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
        let version = super::probe::bin_version("codex").await.unwrap_or_default();
        let override_toml = mcp_override_toml(&shim, &req.socket, &req.token);

        let mut cmd = Command::new("codex");
        if let Some(thread) = &req.resume {
            cmd.args(["exec", "resume", "--json", "--skip-git-repo-check", "-c"])
                .arg(&override_toml)
                .arg("-c")
                .arg("mcp_servers.querora.requires_approval=false")
                .arg(thread);
        } else if self.unsandboxed {
            // CONSENT-GATED (red line): the only codex mode where MCP tools
            // are not auto-cancelled; requires explicit user consent.
            cmd.args([
                "exec",
                "--json",
                "--sandbox",
                "danger-full-access",
                "--skip-git-repo-check",
            ])
            .arg("-c")
            .arg(&override_toml)
            .arg("-c")
            .arg("mcp_servers.querora.requires_approval=false")
            .arg("-c")
            .arg("features.default_mode_request_user_input=false")
            .arg("-");
        } else {
            cmd.args([
                "exec",
                "--json",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "-c",
            ])
            .arg(&override_toml)
            .arg("-");
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(
            if std::env::var("QUERORA_DEBUG_CODEX").is_ok() {
                Stdio::from(
                    std::fs::File::create("/tmp/querora-codex-stderr.log")
                        .unwrap_or_else(|_| std::fs::File::create("/dev/null").expect("devnull")),
                )
            } else {
                Stdio::null()
            },
        );

        let mut child = cmd.spawn().map_err(|e| {
            ToolError::new(ErrorCode::SourceUnavailable, format!("codex spawn: {e}"))
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let mut p = req.prompt.clone();
            p.push('\n');
            stdin.write_all(p.as_bytes()).await.ok();
            stdin.shutdown().await.ok();
        }

        let _ = tx
            .send(AgentEvent::Started {
                agent: "codex".into(),
                session_id: req.resume.clone(),
            })
            .await;

        let stdout = child.stdout.take().expect("stdout piped");
        let mut reader = BufReader::new(stdout).lines();
        let mut outcome = RunOutcome {
            agent_version: version,
            ..Default::default()
        };
        let mut answer_buf = String::new();
        while let Ok(Some(line)) = reader.next_line().await {
            let mut events = Vec::new();
            parse_event_line(&line, &mut events, &mut outcome.session_id);
            for ev in events {
                if let AgentEvent::Token { text } = &ev {
                    answer_buf.push_str(text);
                }
                if let AgentEvent::Answer { .. } = ev {
                    outcome.answered = true;
                    let _ = tx
                        .send(AgentEvent::Answer {
                            text: answer_buf.clone(),
                        })
                        .await;
                } else if tx.send(ev).await.is_err() {
                    break;
                }
            }
        }
        let status = child
            .wait()
            .await
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;

        if !outcome.answered {
            let reason = if req.resume.is_some() {
                AgentEvent::ContextLost {
                    reason: format!(
                        "codex exited without completing (status {status}); the thread may be gone — restate your question"
                    ),
                }
            } else {
                AgentEvent::Failed {
                    error: format!("codex exited without completing (status {status})"),
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
            "x",
            "{",
            "null",
            "42",
            "{\"type\":\"item.completed\"}",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"mcp_tool_call\"}}",
            "{\"type\":\"turn.completed\"}",
        ] {
            let mut out = Vec::new();
            let mut tid = None;
            parse_event_line(garbage, &mut out, &mut tid);
        }
    }

    #[test]
    fn parses_thread_toolcall_and_turn() {
        let mut out = Vec::new();
        let mut tid = None;
        parse_event_line(
            r#"{"type":"thread.started","thread_id":"t42"}"#,
            &mut out,
            &mut tid,
        );
        assert_eq!(tid.as_deref(), Some("t42"));
        parse_event_line(
            r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"hello "}}"#,
            &mut out,
            &mut tid,
        );
        parse_event_line(
            r#"{"type":"item.completed","item":{"id":"i2","type":"mcp_tool_call","server":"querora","tool":"execute_query","arguments":{"ir":{}},"status":"completed","output":[{"type":"text","text":"rows"}]}}"#,
            &mut out,
            &mut tid,
        );
        parse_event_line(r#"{"type":"turn.completed"}"#, &mut out, &mut tid);
        assert!(matches!(&out[0], AgentEvent::Token { text } if text == "hello "));
        assert!(
            matches!(&out[1], AgentEvent::ToolCall { tool, .. } if tool == "mcp__querora__execute_query")
        );
        assert!(matches!(out[3], AgentEvent::Done));
    }

    #[test]
    fn mcp_override_is_valid_toml_value() {
        let toml = mcp_override_toml(
            std::path::Path::new("/usr/local/bin/querora-mcp"),
            std::path::Path::new("/tmp/s.sock"),
            "tok",
        );
        assert!(toml.starts_with("mcp_servers.querora={command="));
        assert!(!toml.contains("requires_approval")); // scalar -c override owns that key
    }
}
