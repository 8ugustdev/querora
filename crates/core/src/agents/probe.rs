// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Capability probing: which agents exist on this machine, which versions.

use querora_contracts::AgentStatus;
use std::process::Stdio;
use tokio::process::Command;

/// Run `<bin> --version` and return the trimmed first line.
pub async fn bin_version(bin: &str) -> Option<String> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let first = s.lines().next().unwrap_or_default().trim();
    if first.is_empty() {
        None
    } else {
        Some(first.to_string())
    }
}

/// Probe claude code.
pub async fn probe_claude() -> AgentStatus {
    match bin_version("claude").await {
        Some(v) => AgentStatus {
            agent: "claude".into(),
            version: Some(v),
            installed: true,
            logged_in: None,
            note: None,
        },
        None => AgentStatus {
            agent: "claude".into(),
            version: None,
            installed: false,
            logged_in: None,
            note: Some("claude not found on PATH".into()),
        },
    }
}

/// Probe codex cli.
pub async fn probe_codex() -> AgentStatus {
    match bin_version("codex").await {
        Some(v) => AgentStatus {
            agent: "codex".into(),
            version: Some(v),
            installed: true,
            logged_in: None,
            note: None,
        },
        None => AgentStatus {
            agent: "codex".into(),
            version: None,
            installed: false,
            logged_in: None,
            note: Some("codex not found on PATH".into()),
        },
    }
}

/// Probe pi (sidecar host).
pub async fn probe_pi() -> AgentStatus {
    match bin_version("pi").await {
        Some(v) => AgentStatus {
            agent: "pi".into(),
            version: Some(v),
            installed: true,
            logged_in: None,
            note: None,
        },
        None => AgentStatus {
            agent: "pi".into(),
            version: None,
            installed: false,
            logged_in: None,
            note: Some("pi not found on PATH".into()),
        },
    }
}

/// Probe all known agents.
pub async fn probe_all() -> Vec<AgentStatus> {
    vec![probe_claude().await, probe_codex().await, probe_pi().await]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probes_never_panic() {
        for s in probe_all().await {
            assert!(!s.agent.is_empty());
        }
    }

    #[tokio::test]
    async fn version_of_missing_binary_is_none() {
        assert!(bin_version("definitely-not-a-real-binary-xyz")
            .await
            .is_none());
    }
}
