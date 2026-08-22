// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! The toolapi server: token-authed JSON-RPC 2.0 over a unix socket.
//!
//! Security model (red-team finding #1):
//! - socket dir `~/.querora/run/` is 0700, owned by the user
//! - the first frame MUST authenticate with a token stored in the Keychain;
//!   foreign/unauthenticated local processes are rejected and audited
//! - every tool call is audited

use super::protocol::{RpcRequest, RpcResponse};
use super::registry::{ToolContext, ToolRegistry};
use crate::keyring::CredentialStore;
use querora_contracts::{ErrorCode, ToolError};
use rand::RngCore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Default socket directory: `~/.querora/run` (0700).
pub fn default_run_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".querora")
        .join("run")
}

/// Keychain account for the toolapi auth token.
pub const TOKEN_ACCOUNT: &str = "toolapi.token";

/// Generate a fresh 256-bit token (hex).
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Token file: `~/.querora/run/toolapi.token` (dir is 0700; file 0600).
///
/// Deviation from the plan (Keychain): unsigned dev builds get a fresh
/// ad-hoc signature on every rebuild, which invalidates the keychain
/// item's ACL and blocks startup on a GUI authorization prompt. A user-
/// owned 0600 file inside the 0700 run dir keeps the same practical
/// boundary (user-only access) without the prompt. The Keychain remains
/// the store for SOURCE credentials. A signed release build may move this
/// back to the Keychain safely.
pub fn token_file() -> PathBuf {
    default_run_dir().join("toolapi.token")
}

/// Get-or-create the toolapi auth token (0600 file; see [`token_file`]).
pub fn get_or_create_token(_creds: &dyn CredentialStore) -> Result<String, ToolError> {
    let path = token_file();
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    let t = generate_token();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    }
    std::fs::write(&path, &t).map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    }
    Ok(t)
}

/// Constant-time token comparison (avoids leaking prefix matches).
fn token_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Running toolapi server handle.
pub struct ToolApiServer {
    socket_path: PathBuf,
    registry: Arc<ToolRegistry>,
    ctx: Arc<ToolContext>,
    token: String,
    /// Optional secondary token (dual mode, `~/.querora/mcp-token`).
    dual_token: std::sync::Mutex<Option<String>>,
}

impl ToolApiServer {
    /// Build a server for `socket_path` (single instance enforced at bind).
    pub fn new(
        socket_path: PathBuf,
        registry: Arc<ToolRegistry>,
        ctx: Arc<ToolContext>,
        token: String,
    ) -> Self {
        let dual = std::fs::read_to_string(crate::dualmode::token_file())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Self {
            socket_path,
            registry,
            ctx,
            token,
            dual_token: std::sync::Mutex::new(dual),
        }
    }

    /// Socket path this server binds.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Enable/rotate dual-mode auth at runtime.
    pub fn set_dual_token(&self, token: Option<String>) {
        *self.dual_token.lock().expect("dual token lock") = token;
    }

    /// Accept loop. Runs until the process exits; call inside `tokio::spawn`.
    pub async fn serve(self: Arc<Self>) -> std::io::Result<()> {
        let dir = self.socket_path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }

        // Single-instance guard: a live socket bound by another instance
        // refuses our connect; a stale socket file gets removed.
        if self.socket_path.exists() {
            match UnixStream::connect(&self.socket_path).await {
                Ok(_) => {
                    return Err(std::io::Error::other(format!(
                        "another Querora instance owns {}",
                        self.socket_path.display()
                    )))
                }
                Err(_) => std::fs::remove_file(&self.socket_path)?,
            }
        }

        let listener = UnixListener::bind(&self.socket_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o600))?;
        }
        tracing::info!("toolapi listening at {}", self.socket_path.display());

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!("toolapi accept error: {e}");
                    continue;
                }
            };
            let server = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(e) = server.handle_connection(stream).await {
                    tracing::debug!("toolapi connection ended: {e}");
                }
            });
        }
    }

    async fn handle_connection(self: Arc<Self>, stream: UnixStream) -> std::io::Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        // --- token handshake on the FIRST frame ---
        let first = match lines.next_line().await {
            Ok(Some(line)) => line,
            _ => return Ok(()),
        };
        let req: RpcRequest = match serde_json::from_str(&first) {
            Ok(r) => r,
            Err(_) => {
                let resp = RpcResponse::err(
                    serde_json::Value::Null,
                    ToolError::new(
                        ErrorCode::Unauthorized,
                        "first frame must be a JSON-RPC auth request",
                    ),
                );
                write_line(&mut writer, &resp).await?;
                return Ok(());
            }
        };
        let presented = req.params["token"].as_str().unwrap_or_default();
        let dual = self.dual_token.lock().expect("dual token lock").clone();
        let authenticated = req.method == "auth"
            && (token_eq(presented, &self.token)
                || dual
                    .as_deref()
                    .map(|d| token_eq(presented, d))
                    .unwrap_or(false));
        let is_dual = authenticated && !token_eq(presented, &self.token);
        if is_dual {
            let _ = self
                .ctx
                .store
                .audit(
                    "dualmode",
                    "auth",
                    "external dual-mode session authenticated",
                )
                .await;
        }
        if !authenticated {
            let _ = self
                .ctx
                .store
                .audit(
                    "toolapi",
                    "auth",
                    "REJECTED unauthenticated or foreign-token connection",
                )
                .await;
            let resp = RpcResponse::err(
                req.id.unwrap_or(serde_json::Value::Null),
                ToolError::new(ErrorCode::Unauthorized, "authentication failed"),
            );
            write_line(&mut writer, &resp).await?;
            return Ok(()); // close
        }
        write_line(
            &mut writer,
            &RpcResponse::ok(serde_json::json!("1"), serde_json::json!({ "ok": true })),
        )
        .await?;

        // --- authenticated request loop ---
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<RpcRequest>(&line) {
                Ok(req) => self.dispatch(req).await,
                Err(e) => RpcResponse::err(
                    serde_json::Value::Null,
                    ToolError::new(ErrorCode::InvalidIr, format!("malformed request: {e}")),
                ),
            };
            write_line(&mut writer, &resp).await?;
        }
        Ok(())
    }

    async fn dispatch(&self, req: RpcRequest) -> RpcResponse {
        let id = req.id.unwrap_or(serde_json::Value::Null);
        if req.method == "list_tools" {
            return RpcResponse::ok(id, serde_json::json!({ "tools": self.registry.describe() }));
        }
        let Some(tool) = self.registry.get(&req.method) else {
            return RpcResponse::err(
                id,
                ToolError::new(
                    ErrorCode::NotFound,
                    format!("unknown tool `{}`", req.method),
                )
                .with_details(serde_json::json!({ "available": self.registry.names() })),
            );
        };
        let tool_name = tool.name();
        let result = tool.handle(req.params, &self.ctx).await;
        let summary = match &result {
            Ok(_) => "ok".to_string(),
            Err(e) => format!("error: {e}"),
        };
        let _ = self.ctx.store.audit("toolapi", tool_name, &summary).await;
        match result {
            Ok(v) => RpcResponse::ok(id, v),
            Err(e) => RpcResponse::err(id, e),
        }
    }
}

async fn write_line<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    resp: &RpcResponse,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(resp).unwrap_or_default();
    line.push('\n');
    writer.write_all(line.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_compare_is_constant_time_and_exact() {
        assert!(token_eq("abcdef", "abcdef"));
        assert!(!token_eq("abcdef", "abcdeX"));
        assert!(!token_eq("abc", "abcd"));
        assert!(!token_eq("", "x"));
    }

    #[test]
    fn generated_tokens_are_unique_and_hex() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
