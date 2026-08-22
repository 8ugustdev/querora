// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Thin toolapi CLIENT: token-authed JSON-RPC over the unix socket.
//! Used by the MCP shim, tests, and mirrors the protocol the pi sidecar
//! speaks from Node.

use super::protocol::{RpcRequest, RpcResponse};
use querora_contracts::{ErrorCode, ToolError};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Connected, authenticated toolapi client.
pub struct ToolApiClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next_id: i64,
}

impl ToolApiClient {
    /// Connect and perform the token handshake.
    pub async fn connect(socket_path: &std::path::Path, token: &str) -> Result<Self, ToolError> {
        let stream = UnixStream::connect(socket_path).await.map_err(|e| {
            ToolError::new(ErrorCode::SourceUnavailable, format!("toolapi socket: {e}"))
        })?;
        let (r, mut w) = stream.into_split();
        let auth = RpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!("auth")),
            method: "auth".into(),
            params: serde_json::json!({ "token": token }),
        };
        let mut line = serde_json::to_string(&auth).unwrap_or_default();
        line.push('\n');
        w.write_all(line.as_bytes())
            .await
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
        let mut reader = BufReader::new(r);
        let mut resp_line = String::new();
        reader
            .read_line(&mut resp_line)
            .await
            .map_err(|e| ToolError::new(ErrorCode::Unauthorized, format!("auth read: {e}")))?;
        let resp: RpcResponse = serde_json::from_str(resp_line.trim_end())
            .map_err(|e| ToolError::new(ErrorCode::Unauthorized, e.to_string()))?;
        if resp.error.is_some() {
            return Err(ToolError::new(
                ErrorCode::Unauthorized,
                "toolapi rejected the token",
            ));
        }
        Ok(Self {
            reader,
            writer: w,
            next_id: 1,
        })
    }

    /// Call a tool; returns the `result` object or a structured `ToolError`.
    pub async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(id)),
            method: method.into(),
            params,
        };
        let mut line = serde_json::to_string(&req).unwrap_or_default();
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
        let mut resp_line = String::new();
        self.reader
            .read_line(&mut resp_line)
            .await
            .map_err(|e| ToolError::new(ErrorCode::Internal, format!("toolapi read: {e}")))?;
        let resp: RpcResponse = serde_json::from_str(resp_line.trim_end()).map_err(|e| {
            ToolError::new(
                ErrorCode::Internal,
                format!("malformed toolapi response: {e}"),
            )
        })?;
        if let Some(err) = resp.error {
            let tool_err: ToolError =
                serde_json::from_value(err.data.unwrap_or(serde_json::Value::Null))
                    .unwrap_or(ToolError::new(ErrorCode::Internal, err.message));
            return Err(tool_err);
        }
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }
}
