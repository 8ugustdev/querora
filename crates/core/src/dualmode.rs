// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Dual mode: expose Querora's MCP server to the user's OWN terminal
//! agents. Security (plan §Phase 8): external processes authenticate with
//! a short-lived localhost token from `~/.querora/mcp-token` (0600);
//! every external query is audited; the app shows live connections.

use crate::storage::AppStore;
use querora_contracts::{ErrorCode, ToolError};
use rand::RngCore;
use std::path::PathBuf;

/// Token file path: `~/.querora/mcp-token` (0600).
pub fn token_file() -> PathBuf {
    crate::toolapi::default_run_dir()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mcp-token")
}

/// Generate a fresh dual-mode token (256-bit hex) and write it 0600.
pub fn rotate_token() -> Result<String, ToolError> {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let path = token_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    }
    std::fs::write(&path, &token)
        .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    }
    Ok(token)
}

/// Validate a presented dual-mode token (constant time).
pub fn validate(presented: &str) -> Result<(), ToolError> {
    let expected = std::fs::read_to_string(token_file())
        .map_err(|_| ToolError::new(ErrorCode::Unauthorized, "dual mode is not enabled"))
        .map(|s| s.trim().to_string())?;
    let a = presented.as_bytes();
    let b = expected.as_bytes();
    let eq = a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0;
    if eq {
        Ok(())
    } else {
        Err(ToolError::new(
            ErrorCode::Unauthorized,
            "invalid dual-mode token",
        ))
    }
}

/// Registration config for claude: `claude mcp add querora -- <shim>` with
/// env carrying the dual-mode token.
pub fn claude_register_command(shim: &std::path::Path, token: &str) -> String {
    format!(
        "claude mcp add querora --env QUERORA_SOCK={} --env QUERORA_DUAL_TOKEN={} -- {}",
        crate::toolapi::default_run_dir()
            .join("querora.sock")
            .display(),
        token,
        shim.display()
    )
}

/// Registration snippet for codex `~/.codex/config.toml` (idempotent merge
/// handled by the caller; content shown to the user for consent).
pub fn codex_config_snippet(shim: &std::path::Path, token: &str) -> String {
    format!(
        "[mcp_servers.querora]\ncommand = \"{}\"\nargs = []\nenv = {{ QUERORA_SOCK = \"{}\", QUERORA_DUAL_TOKEN = \"{}\" }}\nrequires_approval = false\n",
        shim.display(),
        crate::toolapi::default_run_dir().join("querora.sock").display(),
        token
    )
}

/// Record an external (dual-mode) tool call in the audit log.
pub async fn audit_external(store: &AppStore, tool: &str, summary: &str) {
    let _ = store.audit("dualmode", tool, summary).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_and_validate_round_trip() {
        // redirect token file into a temp dir via cwd-independent path:
        // token_file() is fixed; test against the real path but restore after
        let backup = std::fs::read_to_string(token_file()).ok();
        let t = rotate_token().unwrap();
        assert!(validate(&t).is_ok());
        assert!(validate("wrong").is_err());
        assert!(validate("").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(token_file())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "token file must be 0600");
        }
        match backup {
            Some(b) => std::fs::write(token_file(), b).unwrap(),
            None => {
                std::fs::remove_file(token_file()).ok();
            }
        }
    }

    #[test]
    fn snippets_reference_shim_and_token() {
        let shim = std::path::Path::new("/usr/local/bin/querora-mcp");
        let claude = claude_register_command(shim, "tok123");
        assert!(claude.contains("claude mcp add querora"));
        assert!(claude.contains("QUERORA_DUAL_TOKEN=tok123"));
        let codex = codex_config_snippet(shim, "tok123");
        assert!(codex.contains("[mcp_servers.querora]"));
        assert!(codex.contains("requires_approval = false"));
    }
}
