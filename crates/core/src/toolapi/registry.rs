// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Tool registry: the single tool surface shared by the MCP adapter (Phase 5)
//! and the pi sidecar. No business logic lives in adapters — they wrap THIS.

use crate::connectors::DataSources;
use crate::keyring::CredentialStore;
use crate::storage::AppStore;
use async_trait::async_trait;
use querora_contracts::{ErrorCode, QueryResult, SemanticGraph, ToolError};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Shared state handed to every tool invocation.
pub struct ToolContext {
    /// App database.
    pub store: Arc<AppStore>,
    /// Credential store (Keychain in production).
    pub creds: Arc<dyn CredentialStore>,
    /// Live data-source connections (lazily connected, reused).
    pub sources: DataSources,
    /// App-side result cache (full rows never leave this boundary).
    pub results: ResultCache,
    /// The semantic graph the tools serve (Phase 2: fixture; Phase 7: store-backed).
    pub semantic: Mutex<Option<Arc<SemanticGraph>>>,
}

impl ToolContext {
    /// Build a context from parts.
    pub fn new(
        store: Arc<AppStore>,
        creds: Arc<dyn CredentialStore>,
        semantic: Option<Arc<SemanticGraph>>,
    ) -> Self {
        Self {
            store,
            creds,
            sources: DataSources::default(),
            results: ResultCache::default(),
            semantic: Mutex::new(semantic),
        }
    }

    /// Swap the served semantic graph (e.g. after publish).
    pub fn set_semantic(&self, graph: SemanticGraph) {
        *self.semantic.lock().expect("semantic lock poisoned") = Some(Arc::new(graph));
    }

    /// Currently served graph (clone of the Arc).
    pub fn semantic(&self) -> Option<Arc<SemanticGraph>> {
        self.semantic
            .lock()
            .expect("semantic lock poisoned")
            .clone()
    }
}

/// A registered tool. Implementations MUST be side-effect-light and return
/// agent-safe payloads (truncated results, no credentials).
#[async_trait]
pub trait QueroraTool: Send + Sync {
    /// Stable tool name agents call.
    fn name(&self) -> &'static str;

    /// One-paragraph description embedded in MCP tool listings.
    fn description(&self) -> String;

    /// JSON Schema for the params object.
    fn params_schema(&self) -> serde_json::Value;

    /// Execute the tool.
    async fn handle(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<serde_json::Value, ToolError>;
}

/// Ordered registry of tools.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: Arc<Mutex<BTreeMap<&'static str, Arc<dyn QueroraTool>>>>,
}

impl ToolRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool.
    pub fn register(&self, tool: Arc<dyn QueroraTool>) {
        self.tools
            .lock()
            .expect("registry lock poisoned")
            .insert(tool.name(), tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn QueroraTool>> {
        self.tools
            .lock()
            .expect("registry lock poisoned")
            .get(name)
            .cloned()
    }

    /// Tool names in registration order (BTreeMap ⇒ alphabetical).
    pub fn names(&self) -> Vec<&'static str> {
        self.tools
            .lock()
            .expect("registry lock poisoned")
            .keys()
            .copied()
            .collect()
    }

    /// `list_tools` payload: name + description + params schema per tool.
    pub fn describe(&self) -> Vec<serde_json::Value> {
        self.tools
            .lock()
            .expect("registry lock poisoned")
            .values()
            .map(|t| {
                serde_json::json!({
                    "name": t.name(),
                    "description": t.description(),
                    "params_schema": t.params_schema(),
                })
            })
            .collect()
    }
}

/// App-side result cache: full `QueryResult`s keyed by id. The UI reads from
/// here; agents only ever receive the truncated head.
#[derive(Default)]
pub struct ResultCache {
    inner: Mutex<BTreeMap<String, QueryResult>>,
}

impl ResultCache {
    /// Max cached results (FIFO-ish overflow eviction).
    const CAPACITY: usize = 64;

    /// Cache a result.
    pub fn put(&self, result: QueryResult) {
        let mut map = self.inner.lock().expect("cache lock poisoned");
        if map.len() >= Self::CAPACITY {
            if let Some(oldest) = map.keys().next().cloned() {
                map.remove(&oldest);
            }
        }
        map.insert(result.result_id.clone(), result);
    }

    /// Fetch a cached result.
    pub fn get(&self, result_id: &str) -> Option<QueryResult> {
        self.inner
            .lock()
            .expect("cache lock poisoned")
            .get(result_id)
            .cloned()
    }

    /// Current size (diagnostics).
    pub fn len(&self) -> usize {
        self.inner.lock().expect("cache lock poisoned").len()
    }

    /// Whether the cache is empty (diagnostics).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Helper for tools that are scaffolded but not yet wired (progressive
/// build-out across phases).
pub fn not_implemented(what: &str, phase: u8) -> ToolError {
    ToolError::new(
        ErrorCode::NotImplemented,
        format!("{what} is not implemented yet"),
    )
    .with_details(serde_json::json!({ "landing_phase": phase }))
}
