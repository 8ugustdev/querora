// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Structured errors shared across the tool API surface.
//!
//! Every tool failure is returned as a [`ToolError`] with a machine-readable
//! [`ErrorCode`] so agents can self-correct (e.g. `unknown_metric` includes
//! the list of known metric ids in `details`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Machine-readable error codes for tool failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Authentication failed (bad or missing toolapi token).
    Unauthorized,
    /// The requested resource (source, session, result…) does not exist.
    NotFound,
    /// The IR references a metric/dimension unknown to the published graph.
    UnknownMetric,
    /// The IR references a dimension unknown to the published graph.
    UnknownDimension,
    /// Multiple join paths exist and the query must disambiguate.
    AmbiguousJoin,
    /// The IR is malformed or fails validation.
    InvalidIr,
    /// The source is unreachable or misconfigured.
    SourceUnavailable,
    /// The operation is not implemented yet (progressive build-out).
    NotImplemented,
    /// Anything else; see `message`.
    Internal,
}

impl ErrorCode {
    /// JSON-RPC error code (custom range -32000..-32099).
    pub fn rpc_code(self) -> i64 {
        match self {
            Self::Unauthorized => -32001,
            Self::NotFound => -32004,
            Self::UnknownMetric => -32010,
            Self::UnknownDimension => -32011,
            Self::AmbiguousJoin => -32012,
            Self::InvalidIr => -32013,
            Self::SourceUnavailable => -32020,
            Self::NotImplemented => -32050,
            Self::Internal => -32603,
        }
    }
}

/// A structured tool error. `details` carries corrective hints (e.g. the list
/// of valid metric ids for `unknown_metric`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct ToolError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ToolError {
    /// Build an error with just a message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    /// Attach corrective details.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for ToolError {}

impl From<ErrorCode> for ToolError {
    fn from(code: ErrorCode) -> Self {
        let msg = match code {
            ErrorCode::Unauthorized => "authentication failed",
            ErrorCode::NotFound => "not found",
            ErrorCode::UnknownMetric => "unknown metric",
            ErrorCode::UnknownDimension => "unknown dimension",
            ErrorCode::AmbiguousJoin => "ambiguous join path",
            ErrorCode::InvalidIr => "invalid analytical query",
            ErrorCode::SourceUnavailable => "source unavailable",
            ErrorCode::NotImplemented => "not implemented yet",
            ErrorCode::Internal => "internal error",
        };
        Self::new(code, msg)
    }
}
