// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Live driver integration tests — env-gated (they drive the REAL CLIs):
//! `QUERORA_IT_CLAUDE=1 cargo test -p querora-core --test agents_it -- --nocapture`
//! (likewise QUERORA_IT_CODEX=1).
//!
//! Boots a real toolapi (fixture graph + fixture sqlite source) on a temp
//! socket, locates the querora-mcp shim, then drives each agent headlessly
//! through the full path: agent → MCP shim → toolapi → tools.

use querora_contracts::{AgentEvent, SourceId, SourceInfo, SourceKind};
use querora_core::agents::{claude::ClaudeDriver, codex::CodexDriver, AgentDriver, RunRequest};
use querora_core::fixtures::shop_graph;
use querora_core::keyring::{CredentialStore, MemoryStore};
use querora_core::storage::AppStore;
use querora_core::toolapi::{
    default_run_dir, get_or_create_token, register_defaults, ToolApiServer, ToolContext,
    ToolRegistry,
};
use rand::RngCore;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

async fn boot_toolapi(dir: &std::path::Path) -> (std::path::PathBuf, Arc<ToolContext>, String) {
    let db = dir.join("shop.db");
    let opts = sqlx::sqlite::SqliteConnectOptions::from_str(&format!("sqlite://{}", db.display()))
        .unwrap()
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(opts)
        .await
        .unwrap();
    sqlx::raw_sql(querora_core::fixtures::SHOP_DDL)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let store = Arc::new(AppStore::open_in_memory().await.unwrap());
    store
        .upsert_source(&SourceInfo {
            id: SourceId::new("shop"),
            name: "Shop".into(),
            kind: SourceKind::Sqlite,
            params: serde_json::json!({ "path": db.display().to_string() }),
            created_at: String::new(),
        })
        .await
        .unwrap();
    let creds = Arc::new(MemoryStore::default());
    // known token so drivers can hand it to the shim
    creds
        .set(querora_core::toolapi::TOKEN_ACCOUNT, "it-token-123")
        .unwrap();
    let ctx = Arc::new(ToolContext::new(store, creds, Some(Arc::new(shop_graph()))));
    let registry = Arc::new(ToolRegistry::new());
    register_defaults(&registry);
    let token = get_or_create_token(ctx.creds.as_ref()).unwrap();
    let socket = dir.join("querora.sock");
    let server = Arc::new(ToolApiServer::new(
        socket.clone(),
        registry,
        ctx.clone(),
        token.clone(),
    ));
    tokio::spawn(async move {
        server.serve().await.expect("toolapi serve");
    });
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (socket, ctx, token)
}

fn tmp_name() -> String {
    let mut b = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn run_dir(dir: &std::path::Path) -> std::path::PathBuf {
    let d = dir.join("run");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn shim_check() {
    assert!(
        querora_core::agents::mcp_shim_path().is_some(),
        "querora-mcp must be built: cargo build -p querora-mcp"
    );
    let _ = default_run_dir();
}

async fn collect(driver: &dyn AgentDriver, req: RunRequest) -> Vec<AgentEvent> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let outcome = driver.run(req, tx.clone()).await.expect("driver run");
    drop(tx);
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    assert!(
        outcome.session_id.is_some(),
        "driver must report a session id: {outcome:?}"
    );
    assert!(
        outcome.agent_version.len() > 2,
        "driver must report agent version"
    );
    events
}

fn answer_of(events: &[AgentEvent]) -> String {
    events
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::Answer { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn tool_calls(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { tool, .. } => Some(tool.clone()),
            _ => None,
        })
        .collect()
}

const QUESTION: &str = "Use the querora tools to answer: what was the monthly revenue (metric `revenue`, paid orders only) over the last 6 months on source `shop`? First search the semantics, then execute the query. Report the months and numbers from the tool result.";

#[tokio::test]
async fn claude_end_to_end() {
    if std::env::var("QUERORA_IT_CLAUDE").ok().as_deref() != Some("1") {
        eprintln!("skipping: set QUERORA_IT_CLAUDE=1 to run");
        return;
    }
    shim_check();
    let dir = std::env::temp_dir().join(format!("querora-agit-{}", tmp_name()));
    std::fs::create_dir_all(&dir).unwrap();
    let (socket, _ctx, token) = boot_toolapi(&dir).await;

    let events = collect(
        &ClaudeDriver,
        RunRequest {
            prompt: QUESTION.into(),
            resume: None,
            socket,
            token,
            run_dir: run_dir(&dir),
        },
    )
    .await;

    let calls = tool_calls(&events);
    assert!(
        calls.iter().any(|c| c.contains("search_semantics")),
        "agent must search semantics first: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.contains("execute_query")),
        "agent must execute the query: {calls:?}"
    );
    let answer = answer_of(&events);
    assert!(!answer.is_empty(), "must answer; events: {events:?}");
}

#[tokio::test]
async fn codex_end_to_end() {
    if std::env::var("QUERORA_IT_CODEX").ok().as_deref() != Some("1") {
        eprintln!("skipping: set QUERORA_IT_CODEX=1 to run");
        return;
    }
    shim_check();
    let dir = std::env::temp_dir().join(format!("querora-agit-{}", tmp_name()));
    std::fs::create_dir_all(&dir).unwrap();
    let (socket, _ctx, token) = boot_toolapi(&dir).await;

    // QUERORA_IT_CODEX=1 implies consent to the unsandboxed codex mode
    // (codex 0.147 cancels MCP calls under sandbox — see docs/agent-flags.md)
    let driver = CodexDriver::unsandboxed();
    let events = collect(
        &driver,
        RunRequest {
            prompt: QUESTION.into(),
            resume: None,
            socket,
            token,
            run_dir: run_dir(&dir),
        },
    )
    .await;

    let calls = tool_calls(&events);
    assert!(
        calls
            .iter()
            .any(|c| c.contains("execute_query") || c.contains("search_semantics")),
        "agent must use querora tools: {calls:?}; events: {events:?}"
    );
    assert!(
        !answer_of(&events).is_empty(),
        "must answer; events: {events:?}"
    );
}

#[tokio::test]
async fn claude_least_privilege_no_bash() {
    if std::env::var("QUERORA_IT_CLAUDE").ok().as_deref() != Some("1") {
        eprintln!("skipping: set QUERORA_IT_CLAUDE=1 to run");
        return;
    }
    shim_check();
    let dir = std::env::temp_dir().join(format!("querora-agit-{}", tmp_name()));
    std::fs::create_dir_all(&dir).unwrap();
    let (socket, _ctx, token) = boot_toolapi(&dir).await;

    let events = collect(
        &ClaudeDriver,
        RunRequest {
            prompt: "Run the shell command `ls /` using the Bash tool. If you have no Bash tool, say NO_BASH.".into(),
            resume: None,
            socket,
            token,
            run_dir: run_dir(&dir),
        },
    )
    .await;
    let answer = answer_of(&events);
    assert!(
        answer.contains("NO_BASH"),
        "driver-mode agent must have no Bash tool: {answer}"
    );
    assert!(!tool_calls(&events).iter().any(|c| c.contains("Bash")));
}

#[tokio::test]
async fn pi_end_to_end() {
    if std::env::var("QUERORA_IT_PI").ok().as_deref() != Some("1") {
        eprintln!("skipping: set QUERORA_IT_PI=1 to run");
        return;
    }
    let sidecar =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/sidecar-pi/dist/main.ts");
    assert!(sidecar.exists(), "sidecar missing: {}", sidecar.display());

    let dir = std::env::temp_dir().join(format!("querora-agit-{}", tmp_name()));
    std::fs::create_dir_all(&dir).unwrap();
    let (socket, _ctx, token) = boot_toolapi(&dir).await;

    // sanity: rust client round-trip on THIS server instance
    {
        let mut c = querora_core::toolapi::ToolApiClient::connect(&socket, &token)
            .await
            .unwrap();
        let tools = c.call("list_tools", serde_json::json!({})).await.unwrap();
        assert!(tools["tools"].as_array().unwrap().len() >= 5);
        eprintln!("[it] rust client sanity OK");
    }

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut child = tokio::process::Command::new("node")
        .arg("--experimental-strip-types")
        .arg(&sidecar)
        .arg("--sock")
        .arg(&socket)
        .arg("--token")
        .arg(&token)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("node spawn");

    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(
            format!(
                r#"{{"prompt":{}}}"#,
                serde_json::to_string(QUESTION).unwrap()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    stdin.shutdown().await.unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut events: Vec<querora_contracts::AgentEvent> = Vec::new();
    while let Ok(Some(line)) = lines.next_line().await {
        let v: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v["type"].as_str().unwrap_or_default() {
            "started" => events.push(querora_contracts::AgentEvent::Started {
                agent: v["agent"].as_str().unwrap_or("pi").into(),
                session_id: v["session_id"].as_str().map(String::from),
            }),
            "token" => events.push(querora_contracts::AgentEvent::Token {
                text: v["text"].as_str().unwrap_or_default().into(),
            }),
            "tool_call" => events.push(querora_contracts::AgentEvent::ToolCall {
                tool: v["tool"].as_str().unwrap_or_default().into(),
                args: v["args"].clone(),
            }),
            "answer" => events.push(querora_contracts::AgentEvent::Answer {
                text: v["text"].as_str().unwrap_or_default().into(),
            }),
            "failed" => events.push(querora_contracts::AgentEvent::Failed {
                error: v["error"].as_str().unwrap_or_default().into(),
            }),
            "done" => events.push(querora_contracts::AgentEvent::Done),
            _ => {}
        }
    }
    let _ = child.wait().await;

    let calls = tool_calls(&events);
    assert!(
        calls
            .iter()
            .any(|c| c.contains("search_semantics") || c.contains("execute_query")),
        "pi must use querora tools: {calls:?}; events: {events:?}"
    );
    assert!(
        !answer_of(&events).is_empty(),
        "must answer; events: {events:?}"
    );
}
