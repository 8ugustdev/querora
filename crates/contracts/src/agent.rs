// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Agent-facing types: driver events, probe results, run handles.
//! Shared between Rust core and the TS frontend.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One streamed agent event (normalized across claude/codex/pi drivers).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Driver started a run (resume session id when continuing).
    Started {
        /// Agent id (`claude` / `codex` / `pi` / `byok`).
        agent: String,
        /// Driver-native session id, when already known.
        session_id: Option<String>,
    },
    /// Incremental answer text.
    Token {
        /// Text delta.
        text: String,
    },
    /// Agent invoked a Querora tool.
    ToolCall {
        /// Tool name (no `mcp__querora__` prefix).
        tool: String,
        /// Tool arguments.
        args: serde_json::Value,
    },
    /// Tool finished.
    ToolResult {
        /// Tool name.
        tool: String,
        /// Success flag.
        ok: bool,
        /// Short human summary.
        summary: String,
        /// Present for `execute_query`: cache key the UI uses to fetch the
        /// full result (charts/trust panel) app-side.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_id: Option<String>,
    },
    /// Final answer text.
    Answer {
        /// Complete answer.
        text: String,
    },
    /// Resume failed — earlier context may be lost (agent upgraded etc.).
    ContextLost {
        /// Why.
        reason: String,
    },
    /// Run failed.
    Failed {
        /// Error description.
        error: String,
    },
    /// Stream finished.
    Done,
}

/// Per-agent capability probe result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct AgentStatus {
    /// Agent id.
    pub agent: String,
    /// `claude --version`-style version string, when installed.
    pub version: Option<String>,
    /// Binary found on PATH.
    pub installed: bool,
    /// Logged in / authenticated (None = not verified cheaply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logged_in: Option<bool>,
    /// Human note (e.g. failure reason).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
