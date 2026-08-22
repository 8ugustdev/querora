// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors
//
//! `querora-mcp` — MCP stdio server bridging Claude Code / Codex to the
//! Querora toolapi unix socket.
//!
//! Protocol subset (spike-validated against claude 2.x and codex 0.147):
//! `initialize` → `notifications/initialized` → `tools/list` /
//! `tools/call` (+ `ping`). NDJSON both ways on stdio; logs to stderr only.
//!
//! Config via env (injected by the app through the mcp-config file, 0600):
//! - `QUERORA_SOCK`   unix socket path (default ~/.querora/run/querora.sock)
//! - `QUERORA_TOKEN`  toolapi auth token (never argv)

use querora_contracts::ToolError;
use querora_core::toolapi::ToolApiClient;
use std::io::Write;
use std::path::PathBuf;

/// IR cheat-sheet appended to every tool description — agents never guess
/// the IR shape.
const IR_CHEATSHEET: &str = "\n\nQuerora workflow: 1) search_semantics to map the question to metric/dimension ids; 2) dry_run to validate; 3) execute_query with AnalyticalQuery IR {source, measures:[{metric_id}], dimensions:[{dimension_id, grain?}], filters:[{dimension_id, op, value?}], time?, order?, limit?}. NEVER write SQL.";

fn sock_path() -> PathBuf {
    std::env::var("QUERORA_SOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|_| querora_core::toolapi::default_run_dir().join("querora.sock"))
}

fn token() -> Result<String, String> {
    std::env::var("QUERORA_TOKEN")
        .map_err(|_| "QUERORA_TOKEN not set (spawn me via Querora's mcp-config)".into())
}

struct Io {
    writer: tokio::io::Stdout,
}

impl Io {
    async fn send(&mut self, v: &serde_json::Value) {
        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_string(v).unwrap_or_default();
        line.push('\n');
        let _ = self.writer.write_all(line.as_bytes()).await;
    }
}

fn result(id: &serde_json::Value, r: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": r })
}

fn error(id: &serde_json::Value, code: i64, msg: &str) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg } })
}

fn tool_result_error(err: &ToolError) -> serde_json::Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(err).unwrap_or_else(|_| err.to_string()) }],
        "isError": true,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run().await {
        let mut err = std::io::stderr();
        let _ = writeln!(err, "querora-mcp: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let token = token()?;
    let path = sock_path();
    let mut client = ToolApiClient::connect(&path, &token)
        .await
        .map_err(|e| format!("toolapi connect failed: {e}"))?;

    let w = tokio::io::stdout();
    let mut io = Io { writer: w };

    // stdin is blocking — read lines on a plain thread, forward via channel
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(64);
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        let mut line = String::new();
        loop {
            line.clear();
            match lock.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if line_tx.blocking_send(line.clone()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    while let Some(line) = line_rx.recv().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = req["method"].as_str().unwrap_or_default().to_string();

        match method.as_str() {
            "initialize" => {
                io.send(&result(
                    &id,
                    serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "querora", "version": env!("CARGO_PKG_VERSION") }
                    }),
                ))
                .await;
            }
            "notifications/initialized" | "initialized" => {}
            "ping" => {
                io.send(&result(&id, serde_json::json!({}))).await;
            }
            "tools/list" => match client.call("list_tools", serde_json::json!({})).await {
                Ok(v) => {
                    let tools: Vec<serde_json::Value> = v["tools"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|mut t| {
                            if let Some(d) = t["description"].as_str() {
                                t["description"] =
                                    serde_json::Value::String(format!("{d}{IR_CHEATSHEET}"));
                            }
                            t["inputSchema"] = t
                                .get("params_schema")
                                .cloned()
                                .unwrap_or(serde_json::json!({"type": "object"}));
                            t
                        })
                        .collect();
                    io.send(&result(&id, serde_json::json!({ "tools": tools })))
                        .await;
                }
                Err(e) => {
                    io.send(&error(&id, -32000, &e.to_string())).await;
                }
            },
            "tools/call" => {
                let tool = req["params"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let args = req["params"]["arguments"].clone();
                match client.call(&tool, args).await {
                    Ok(v) => {
                        io.send(&result(
                            &id,
                            serde_json::json!({
                                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
                            }),
                        ))
                        .await;
                    }
                    Err(e) => {
                        io.send(&result(&id, tool_result_error(&e))).await;
                    }
                }
            }
            "" => {}
            other => {
                if !id.is_null() {
                    io.send(&error(&id, -32601, &format!("unknown method {other}")))
                        .await;
                }
            }
        }
    }
    Ok(())
}
