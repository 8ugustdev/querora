// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! pi agent driver: runs the Node sidecar (`apps/sidecar-pi`) which hosts
//! a pi SDK session with ONLY Querora tools. Model + thinking effort are
//! app settings (defaults `zai/glm-5.3` + `medium`) passed to the sidecar
//! as a settings JSON file (never argv-visible secrets — settings carry
//! model ids only, which are not secret).

use super::{kill_tree, AgentDriver, RunOutcome, RunRequest};
use async_trait::async_trait;
use querora_contracts::{AgentEvent, AgentStatus, ErrorCode, ToolError};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// Driver over the pi sidecar.
pub struct PiDriver {
    /// Model id (e.g. `zai/glm-5.3`).
    pub model: String,
    /// Thinking effort (off|minimal|low|medium|high|xhigh|max).
    pub effort: String,
    /// Chat session id — drives a stable `.jsonl` session file so later
    /// turns resume the same pi conversation (`resume` in RunRequest).
    pub chat_session: String,
}

impl PiDriver {
    /// Driver with app settings.
    pub fn new(model: impl Into<String>, effort: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            effort: effort.into(),
            chat_session: String::new(),
        }
    }

    /// Bind to a chat session id (enables multi-turn resume).
    pub fn for_chat(mut self, chat_session: impl Into<String>) -> Self {
        self.chat_session = chat_session.into();
        self
    }
}

impl Default for PiDriver {
    fn default() -> Self {
        Self::new("zai/glm-5.3", "medium")
    }
}

/// Locate the sidecar entrypoint: repo `apps/sidecar-pi/dist/main.ts`
/// (dev, run via `node --experimental-strip-types`).
pub fn sidecar_entry() -> Option<std::path::PathBuf> {
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        // crates/core/../../apps/sidecar-pi
        let p = std::path::Path::new(&manifest).join("../../apps/sidecar-pi/dist/main.ts");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Parse one sidecar JSONL event (mirrors AgentEvent) — never panics.
pub fn parse_event_line(line: &str, events: &mut Vec<AgentEvent>, session_id: &mut Option<String>) {
    let v: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return,
    };
    let ty = v["type"].as_str().unwrap_or_default();
    match ty {
        "started" => {
            *session_id = v["session_id"].as_str().map(str::to_string);
            events.push(AgentEvent::Started {
                agent: "pi".into(),
                session_id: session_id.clone(),
            });
        }
        "token" => {
            if let Some(t) = v["text"].as_str() {
                if !t.is_empty() {
                    events.push(AgentEvent::Token {
                        text: t.to_string(),
                    });
                }
            }
        }
        "tool_call" => events.push(AgentEvent::ToolCall {
            tool: v["tool"].as_str().unwrap_or_default().to_string(),
            args: v["args"].clone(),
        }),
        "answer" => {
            if let Some(t) = v["text"].as_str() {
                events.push(AgentEvent::Answer {
                    text: t.to_string(),
                });
            }
        }
        "context_lost" => events.push(AgentEvent::ContextLost {
            reason: v["reason"].as_str().unwrap_or_default().to_string(),
        }),
        "failed" => events.push(AgentEvent::Failed {
            error: v["error"].as_str().unwrap_or_default().to_string(),
        }),
        "done" => events.push(AgentEvent::Done),
        _ => {}
    }
}

#[async_trait]
impl AgentDriver for PiDriver {
    fn id(&self) -> &'static str {
        "pi"
    }

    async fn probe(&self) -> AgentStatus {
        let mut s = super::probe::probe_pi().await;
        if sidecar_entry().is_none() {
            s.note = Some("pi sidecar not built (apps/sidecar-pi/dist/main.ts missing)".into());
        }
        s
    }

    async fn run(
        &self,
        req: RunRequest,
        tx: mpsc::Sender<AgentEvent>,
    ) -> Result<RunOutcome, ToolError> {
        let entry = sidecar_entry().ok_or_else(|| {
            ToolError::new(
                ErrorCode::SourceUnavailable,
                "pi sidecar not built (apps/sidecar-pi/dist/main.ts)",
            )
        })?;
        let version = super::probe::bin_version("pi").await.unwrap_or_default();

        // settings file (model ids only — not secret); reused per pid
        let settings_path = req
            .run_dir
            .join(format!("pi-settings-{}.json", std::process::id()));
        std::fs::write(
            &settings_path,
            serde_json::json!({ "model": self.model, "thinkingLevel": self.effort }).to_string(),
        )
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;

        let mut cmd = Command::new("node");
        cmd.arg("--experimental-strip-types")
            .arg(&entry)
            .arg("--sock")
            .arg(&req.socket)
            .arg("--token")
            .arg(&req.token)
            .arg("--settings")
            .arg(&settings_path);
        // multi-turn in pi's NATIVE store: first turn = "new", later turns =
        // resume by uuid (mirrors `pi --session <id>`). We save only the id;
        // the .jsonl lives wherever pi keeps its sessions.
        let resume_id = req.resume.clone().unwrap_or_else(|| "new".to_string());
        cmd.arg("--session-id").arg(&resume_id);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                ToolError::new(ErrorCode::SourceUnavailable, format!("node spawn: {e}"))
            })?;

        // single-turn: send prompt, read until answer/done/EOF
        if let Some(mut stdin) = child.stdin.take() {
            let mut line = serde_json::json!({ "prompt": req.prompt }).to_string();
            line.push('\n');
            stdin.write_all(line.as_bytes()).await.ok();
            stdin.shutdown().await.ok();
        }

        let stdout = child.stdout.take().expect("stdout piped");
        let mut reader = BufReader::new(stdout).lines();
        let mut outcome = RunOutcome {
            agent_version: version,
            ..Default::default()
        };
        let mut answer_buf = String::new();
        let mut saw_done = false;
        while let Ok(Some(line)) = reader.next_line().await {
            let mut events = Vec::new();
            parse_event_line(&line, &mut events, &mut outcome.session_id);
            for ev in events {
                if let AgentEvent::Token { text } = &ev {
                    answer_buf.push_str(text);
                }
                if let AgentEvent::Answer { .. } = &ev {
                    outcome.answered = true;
                }
                if matches!(ev, AgentEvent::Done) {
                    saw_done = true;
                }
                let _ = tx.send(ev).await;
            }
            if saw_done {
                break;
            }
        }
        let status = child
            .wait()
            .await
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;

        if !outcome.answered && !answer_buf.is_empty() {
            // stream ended without an explicit answer event — synthesize one
            let _ = tx
                .send(AgentEvent::Answer {
                    text: answer_buf.clone(),
                })
                .await;
            outcome.answered = true;
        }
        if !outcome.answered {
            let _ = tx
                .send(AgentEvent::Failed {
                    error: format!("pi sidecar exited without an answer (status {status})"),
                })
                .await;
        }
        let _ = tx.send(AgentEvent::Done).await;
        kill_tree(&mut child).await;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_lines_never_panic() {
        for garbage in ["", "x", "{", "null", "{\"type\":\"wizard\"}"] {
            let mut out = Vec::new();
            let mut sid = None;
            parse_event_line(garbage, &mut out, &mut sid);
        }
    }

    #[test]
    fn parses_full_turn() {
        let mut out = Vec::new();
        let mut sid = None;
        parse_event_line(
            r#"{"type":"started","agent":"pi","session_id":"s1"}"#,
            &mut out,
            &mut sid,
        );
        parse_event_line(r#"{"type":"token","text":"hello "}"#, &mut out, &mut sid);
        parse_event_line(
            r#"{"type":"tool_call","tool":"search_semantics","args":{"query":"revenue"}}"#,
            &mut out,
            &mut sid,
        );
        parse_event_line(
            r#"{"type":"answer","text":"hello world"}"#,
            &mut out,
            &mut sid,
        );
        parse_event_line(r#"{"type":"done"}"#, &mut out, &mut sid);
        assert_eq!(sid.as_deref(), Some("s1"));
        assert!(matches!(&out[1], AgentEvent::Token { text } if text == "hello "));
        assert!(matches!(&out[2], AgentEvent::ToolCall { tool, .. } if tool == "search_semantics"));
        assert!(matches!(&out[3], AgentEvent::Answer { text } if text == "hello world"));
        assert!(matches!(out[4], AgentEvent::Done));
    }
}
