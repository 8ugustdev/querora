//! Live end-to-end: natural-language question through the real claude
//! driver against the RUNNING app's toolapi (live graph + MySQL).
//!
//! `QUERORA_AGENT_LIVE=1 cargo test -p querora-core --test agent_live
//! -- --nocapture --ignored`

use querora_contracts::AgentEvent;
use querora_core::agents::claude::ClaudeDriver;
use querora_core::agents::{AgentDriver, RunRequest};
use std::path::PathBuf;

#[tokio::test]
#[ignore]
async fn margin_vs_last_year_natural_language() {
    if std::env::var("QUERORA_AGENT_LIVE").ok().as_deref() != Some("1") {
        eprintln!("skipping: set QUERORA_AGENT_LIVE=1");
        return;
    }
    let home = std::env::var("HOME").unwrap();
    let run_dir = PathBuf::from(&home).join(".querora/run");
    let token = std::fs::read_to_string(run_dir.join("toolapi.token"))
        .expect("toolapi.token")
        .trim()
        .to_string();
    let req = RunRequest {
        prompt: "How was the margin compared to last year?".into(),
        resume: None,
        socket: run_dir.join("querora.sock"),
        token,
        run_dir: run_dir.clone(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let handle = tokio::spawn(async move { ClaudeDriver.run(req, tx).await });
    while let Some(ev) = rx.recv().await {
        match &ev {
            AgentEvent::Token { text } => print!("{text}"),
            AgentEvent::ToolCall { tool, .. } => {
                eprintln!("\n[tool] {tool}")
            }
            _ => eprintln!("[event] {ev:?}"),
        }
    }
    let outcome = handle.await.unwrap().expect("driver run");
    println!("\n[outcome] {outcome:?}");
}
