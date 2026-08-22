// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! token-authed unix-socket JSON-RPC tool API.

pub mod client;
pub mod protocol;
pub mod registry;
pub mod server;
pub mod tools;

pub use client::ToolApiClient;
pub use protocol::{RpcRequest, RpcResponse};
pub use registry::{QueroraTool, ToolContext, ToolRegistry};
pub use server::{
    default_run_dir, generate_token, get_or_create_token, ToolApiServer, TOKEN_ACCOUNT,
};
pub use tools::{
    register_defaults, ExecuteQueryTool, GetSchemaTool, ProfileColumnTool, SearchSemanticsParams,
    SearchSemanticsTool,
};
