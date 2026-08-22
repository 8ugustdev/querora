// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Agent drivers: spawn the user's CLI agents headlessly, scoped to
//! Querora tools, streaming normalized [`AgentEvent`]s.
//!
//! Security model (red-team findings #4, #5, #10):
//! - least-privilege: driver-mode agents get ONLY Querora tools
//!   (claude `--disallowedTools` defaults; codex sandbox read-only +
//!   `requires_approval=false` only for the querora server; pi
//!   `noTools: "builtin"`)
//! - the toolapi token reaches shims via a 0600 mcp-config file the app
//!   writes (never argv; never the app db); the pi sidecar receives it
//!   via fd 3
//! - the app owns the process tree and kills it on conversation end

pub mod claude;
pub mod codex;
pub mod pi;
pub mod probe;
pub mod session;

use async_trait::async_trait;
use querora_contracts::{AgentEvent, AgentStatus, ToolError};
use std::path::PathBuf;
use tokio::sync::mpsc;

/// Everything a driver needs to run one conversation turn.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// User prompt for this turn.
    pub prompt: String,
    /// Driver-native session id to resume, when continuing.
    pub resume: Option<String>,
    /// toolapi unix socket path.
    pub socket: PathBuf,
    /// toolapi auth token.
    pub token: String,
    /// Scratch dir for per-session configs (0700): `~/.querora/run`.
    pub run_dir: PathBuf,
}

/// How the turn ended.
#[derive(Debug, Clone, Default)]
pub struct RunOutcome {
    /// Driver-native session id for resume.
    pub session_id: Option<String>,
    /// Agent version observed.
    pub agent_version: String,
    /// True when the answer streamed successfully.
    pub answered: bool,
}

/// Uniform driver trait — swapping agents is config, not code.
#[async_trait]
pub trait AgentDriver: Send + Sync {
    /// Stable agent id (`claude`, `codex`, `pi`).
    fn id(&self) -> &'static str;

    /// Cheap capability probe (installed? version? logged in?).
    async fn probe(&self) -> AgentStatus;

    /// Run one turn, streaming events into `tx` until done.
    async fn run(
        &self,
        req: RunRequest,
        tx: mpsc::Sender<AgentEvent>,
    ) -> Result<RunOutcome, ToolError>;
}

/// Kill a process tree (job-object semantics): child + grandchildren.
/// macOS: `kill -TERM -<pgid>` via spawning the child in its own process
/// group is the portable approach we use.
pub(crate) async fn kill_tree(child: &mut tokio::process::Child) {
    child.start_kill().ok();
    let _ = child.wait().await;
}

/// Locate the `querora-mcp` shim binary: sibling of the current executable
/// (dev: target/{debug,release}; app bundle: Contents/MacOS), else PATH.
pub fn mcp_shim_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if let Some(dir) = exe.parent() {
        let sibling = dir.join("querora-mcp");
        if sibling.exists() {
            return Some(sibling);
        }
    }
    // dev fallback: cargo target dir from manifest-relative layout
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        // crates/core/../../target/{debug}/querora-mcp
        for profile in ["debug", "release"] {
            let p = std::path::Path::new(&manifest)
                .join("../../target")
                .join(profile)
                .join("querora-mcp");
            if p.exists() {
                return Some(p);
            }
        }
    }
    which::which("querora-mcp").ok()
}
