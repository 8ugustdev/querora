// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! JSON-RPC 2.0 protocol types for the toolapi (NDJSON framing).

use querora_contracts::ToolError;
use serde::{Deserialize, Serialize};

/// A JSON-RPC 2.0 request. The first frame on a connection MUST have
/// `method == "auth"` and `params == { "token": … }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    /// `"2.0"`.
    pub jsonrpc: String,
    /// Request id (echoed back). String or number.
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    /// Method name: `"auth"` or a registered tool name.
    pub method: String,
    /// Method parameters.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    /// Machine-readable code (see `ErrorCode::rpc_code`).
    pub code: i64,
    /// Human-readable message.
    pub message: String,
    /// Structured `ToolError` payload when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl From<ToolError> for RpcError {
    fn from(err: ToolError) -> Self {
        Self {
            code: err.code.rpc_code(),
            message: err.message.clone(),
            data: Some(serde_json::to_value(&err).unwrap_or(serde_json::Value::Null)),
        }
    }
}

/// A JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    /// `"2.0"`.
    pub jsonrpc: String,
    /// Echoed request id.
    pub id: serde_json::Value,
    /// Result on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    /// Successful response.
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Error response.
    pub fn err(id: serde_json::Value, err: ToolError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(RpcError::from(err)),
        }
    }
}
