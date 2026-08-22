// SPDX-License-Identifier: Apache-2.0
//! Querora desktop app shell.
//!
//! Mounts the core (app db, keyring, toolapi server) and exposes Tauri
//! commands: chat (streaming agent events), sessions, agent probes,
//! source CRUD, full-result fetch + CSV export.

use std::sync::Arc;
use tauri::{Emitter, Manager};

/// Bridge probe used by the frontend `tauri-bridge.ts`.
#[tauri::command]
fn ping() -> &'static str {
    "pong"
}

/// Runtime status of the mounted core (toolapi socket, served tools).
#[tauri::command]
async fn querora_status(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
) -> Result<serde_json::Value, String> {
    let socket = querora_core::toolapi::default_run_dir().join("querora.sock");
    Ok(serde_json::json!({
        "socket_path": socket.display().to_string(),
        "semantic_version": state.semantic().map(|g| g.version.clone()),
        "source": state.semantic().map(|g| g.source.0.clone()),
    }))
}

/// Probe all known CLI agents (installed/version).
#[tauri::command]
async fn probe_agents() -> Result<Vec<querora_contracts::AgentStatus>, String> {
    Ok(querora_core::agents::probe::probe_all().await)
}

/// List chat sessions (newest first).
#[tauri::command]
async fn list_sessions(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
) -> Result<Vec<SessionDto>, String> {
    Ok(state
        .store
        .list_sessions()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|r| SessionDto {
            id: r.id,
            agent: r.agent,
            title: r.title,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect())
}

use querora_contracts::AgentEvent;

/// One chat session row (frontend shape).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    pub id: String,
    pub agent: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Where the app keeps its state (created on demand): `~/.querora`.
fn querora_home() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".querora")
}

/// Mount the core: app db, credential store, toolapi server (single instance).
fn mount_core(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use querora_core::toolapi::{register_defaults, ToolApiServer, ToolContext, ToolRegistry};

    let home = querora_home();
    std::fs::create_dir_all(&home)?;

    let store = tauri::async_runtime::block_on(querora_core::storage::AppStore::open(
        &home.join("app.db"),
    ))?;
    let store = Arc::new(store);
    let creds = Arc::from(querora_core::keyring::default_credential_store());

    // serve the LATEST PUBLISHED graph (any source); fixture is only a demo fallback
    let served: Option<Arc<querora_contracts::SemanticGraph>> =
        tauri::async_runtime::block_on(async {
            let sources = store.list_sources().await.unwrap_or_default();
            for s in &sources {
                if let Ok(Some(g)) = store.published_graph(&s.id).await {
                    return Some(Arc::new(g));
                }
            }
            None
        });
    let ctx = Arc::new(ToolContext::new(
        store,
        creds,
        Some(served.unwrap_or_else(|| Arc::new(querora_core::fixtures::shop_graph()))),
    ));
    app.manage(ctx.clone());

    let registry = Arc::new(ToolRegistry::new());
    register_defaults(&registry);

    let socket = querora_core::toolapi::default_run_dir().join("querora.sock");
    let token = querora_core::toolapi::get_or_create_token(ctx.creds.as_ref())
        .map_err(|e| format!("toolapi token: {e}"))?;
    let server = Arc::new(ToolApiServer::new(socket, registry, ctx, token));
    app.manage(server.clone());

    tauri::async_runtime::spawn(async move {
        if let Err(e) = server.serve().await {
            tracing::error!("toolapi server stopped: {e} — is another Querora instance running?");
        }
    });
    Ok(())
}

/// Run one agent turn, streaming `AgentEvent`s to the frontend as
/// `agent-event://<session_id>` Tauri events. Persists the exchange.
#[tauri::command]
async fn chat_send(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    session_id: String,
    agent: String,
    prompt: String,
) -> Result<serde_json::Value, String> {
    let prompt_snapshot = prompt.clone();
    let socket = querora_core::toolapi::default_run_dir().join("querora.sock");
    let token = querora_core::toolapi::get_or_create_token(state.creds.as_ref())
        .map_err(|e| e.to_string())?;
    let run_dir = querora_core::toolapi::default_run_dir();
    std::fs::create_dir_all(&run_dir).ok();

    let resume = querora_core::agents::session::load_session(&state.store, &session_id)
        .await
        .ok()
        .flatten()
        .and_then(|(_agent, sid, _ver)| sid);

    let prefs: AgentPrefs = match state.store.get_setting(PREFS_KEY).await {
        Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
        _ => AgentPrefs::default(),
    };
    let driver: Arc<dyn querora_core::agents::AgentDriver> = match agent.as_str() {
        "claude" => Arc::new(querora_core::agents::claude::ClaudeDriver),
        "codex" => Arc::new(querora_core::agents::codex::CodexDriver::new()),
        "pi" => Arc::new(
            querora_core::agents::pi::PiDriver::new(&prefs.pi_model, &prefs.pi_effort)
                .for_chat(&session_id),
        ),
        _ => return Err(format!("unknown agent `{agent}`")),
    };

    // persist the user's turn BEFORE running
    let _ = store_create_if_missing(&state.store, &session_id, &agent, "").await;
    let _ = state
        .store
        .append_message(&session_id, "user", &serde_json::json!({ "text": prompt }))
        .await;

    let req = querora_core::agents::RunRequest {
        prompt,
        socket,
        token,
        run_dir,
        resume,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
    let store = state.store.clone();
    let session_id2 = session_id.clone();

    let emit_app = app.clone();
    let sid = session_id.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let mut events: Vec<AgentEvent> = Vec::new();
        while let Some(ev) = rx.recv().await {
            let _ = emit_app.emit(format!("agent-event://{sid}").as_str(), &ev);
            events.push(ev);
        }
        events
    });

    let outcome = driver.run(req, tx).await.map_err(|e| e.to_string())?;

    let _ = store
        .set_session_title_if_empty(&session_id2, &prompt_snapshot)
        .await;
    // persist session handle + agent-side exchange (no duplicate user row)
    let _ = store
        .set_session_agent(
            &session_id2,
            outcome.session_id.as_deref(),
            Some(&outcome.agent_version),
        )
        .await;
    let _prompt_echo = prompt_snapshot.clone();
    let events = handle.await.unwrap_or_default();
    let _ = append_agent_exchange(&store, &session_id2, &events).await;

    Ok(serde_json::json!({ "ok": outcome.answered, "session_id": session_id2 }))
}

async fn store_create_if_missing(
    store: &Arc<querora_core::storage::AppStore>,
    id: &str,
    agent: &str,
    title: &str,
) {
    let _ = store.create_session_if_missing(id, agent, title).await;
}

/// Persist the agent answer + tool timeline (user turn stored separately).
async fn append_agent_exchange(
    store: &Arc<querora_core::storage::AppStore>,
    session_id: &str,
    events: &[AgentEvent],
) -> Result<(), String> {
    let answer = events
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::Answer { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let tool_timeline: Vec<serde_json::Value> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { tool, args } => {
                Some(serde_json::json!({ "kind": "call", "tool": tool, "args": args }))
            }
            AgentEvent::ToolResult { tool, ok, summary, result_id } => Some(serde_json::json!({
                "kind": "result", "tool": tool, "ok": ok, "summary": summary, "result_id": result_id,
            })),
            AgentEvent::ContextLost { reason } => {
                Some(serde_json::json!({ "kind": "context_lost", "reason": reason }))
            }
            _ => None,
        })
        .collect();
    store
        .append_message(
            session_id,
            "agent",
            &serde_json::json!({ "text": answer, "tool_timeline": tool_timeline }),
        )
        .await
        .map_err(|e| e.to_string())
}

/// Record the user's prompt text (called before chat_send resolves).
#[tauri::command]
async fn append_user_message(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    session_id: String,
    text: String,
) -> Result<(), String> {
    state
        .store
        .append_message(&session_id, "user", &serde_json::json!({ "text": text }))
        .await
        .map_err(|e| e.to_string())
}

/// Fetch a cached full result by id (UI hop-over from the trust panel).
#[tauri::command]
async fn get_result(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    result_id: String,
) -> Result<Option<querora_contracts::QueryResult>, String> {
    Ok(state.results.get(&result_id))
}

/// Export a cached result to CSV (returns the file path written).
#[tauri::command]
async fn export_csv(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    result_id: String,
) -> Result<String, String> {
    let result = state
        .results
        .get(&result_id)
        .ok_or_else(|| format!("result `{result_id}` no longer cached"))?;
    let mut csv = String::new();
    csv.push_str(&result.columns.join(","));
    csv.push('\n');
    for row in &result.rows {
        let cells: Vec<String> = result
            .columns
            .iter()
            .map(|c| {
                let v = &row[c];
                match v {
                    serde_json::Value::String(s) => format!("\"{}\"", s.replace('"', "\"\"")),
                    serde_json::Value::Null => String::new(),
                    other => other.to_string(),
                }
            })
            .collect();
        csv.push_str(&cells.join(","));
        csv.push('\n');
    }
    let path = querora_home()
        .join("exports")
        .join(format!("{result_id}.csv"));
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, csv).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

/// Source CRUD: list.
#[tauri::command]
async fn list_sources(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
) -> Result<Vec<querora_contracts::SourceInfo>, String> {
    state.store.list_sources().await.map_err(|e| e.to_string())
}

/// Source CRUD: add (secret goes to the Keychain, never SQLite).
#[tauri::command]
async fn add_source(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    info: querora_contracts::SourceInfo,
    secret: Option<String>,
) -> Result<(), String> {
    if let Some(secret) = secret {
        state
            .creds
            .set(&querora_core::connectors::secret_account(&info.id), &secret)
            .map_err(|e| e.to_string())?;
    }
    state
        .store
        .upsert_source(&info)
        .await
        .map_err(|e| e.to_string())
}

/// Source CRUD: test connection (introspect catalog head).
#[tauri::command]
async fn test_source(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    info: querora_contracts::SourceInfo,
    secret: Option<String>,
) -> Result<serde_json::Value, String> {
    let existing = state
        .creds
        .get(&querora_core::connectors::secret_account(&info.id))
        .ok()
        .flatten()
        .unwrap_or_default();
    let secret = secret.unwrap_or(existing);
    let ds = querora_core::connectors::connect(&info, &secret)
        .await
        .map_err(|e| e.message)?;
    let cat = ds.catalog().await.map_err(|e| e.message)?;
    Ok(
        serde_json::json!({ "ok": true, "tables": cat.tables.len(), "dialect": format!("{:?}", ds.dialect()).to_lowercase() }),
    )
}

/// Source CRUD: remove (invalidates connections, deletes secret).
#[tauri::command]
async fn remove_source(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    id: String,
) -> Result<(), String> {
    let sid = querora_contracts::SourceId::new(id);
    state.sources.invalidate(&sid).await;
    let _ = state
        .creds
        .delete(&querora_core::connectors::secret_account(&sid));
    state
        .store
        .delete_source(&sid)
        .await
        .map_err(|e| e.to_string())
}

/// Semantic: generate heuristic draft for a source (one-click).
#[tauri::command]
async fn draft_semantics(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    source: String,
) -> Result<serde_json::Value, String> {
    let id = querora_contracts::SourceId::new(source);
    let ds = state
        .sources
        .get(&id, &state.store, state.creds.as_ref())
        .await
        .map_err(|e| e.message)?;
    let catalog = ds.catalog().await.map_err(|e| e.message)?;
    state
        .store
        .set_catalog(&id, &catalog)
        .await
        .map_err(|e| e.to_string())?;
    let mut sug = querora_core::semantic::suggest(&id, &catalog);
    // Magento EAV unfolding: flatten brand/cost/order-date into a virtual
    // order_line entity + filter-only category entity (semi-join).
    let mut eav_note = String::new();
    if let Some(info) = querora_core::semantic::detect(ds.as_ref(), &catalog).await {
        let hints = querora_core::semantic::fetch_value_hints(ds.as_ref(), &info).await;
        let ext = querora_core::semantic::build_extension(&id, &info);
        querora_core::semantic::merge_eav(&mut sug.graph, &ext);
        sug.graph.value_index = hints;
        eav_note = " (+ Magento EAV: order_line, brand, cost, category)".to_string();
    }
    state
        .store
        .save_draft(&id, &sug.graph)
        .await
        .map_err(|e| e.to_string())?;
    // keep FTS warm for search_semantics (draft content searchable too)
    querora_core::semantic::index_graph(&state.store, &sug.graph)
        .await
        .map_err(|e| e.message)?;
    Ok(serde_json::json!({
        "graph": sug.graph,
        "unjoined_tables": sug.unjoined_tables,
        "candidate_relationships": sug.candidate_relationships,
        "eav": eav_note,
    }))
}

/// Semantic: fetch latest draft.
#[tauri::command]
async fn latest_draft(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    source: String,
) -> Result<Option<querora_contracts::SemanticGraph>, String> {
    state
        .store
        .latest_draft(&querora_contracts::SourceId::new(source))
        .await
        .map_err(|e| e.to_string())
        .map(|row| row.map(|r| r.graph))
}

/// Semantic: fetch published graph.
#[tauri::command]
async fn published_graph(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    source: String,
) -> Result<Option<querora_contracts::SemanticGraph>, String> {
    state
        .store
        .published_graph(&querora_contracts::SourceId::new(source))
        .await
        .map_err(|e| e.to_string())
}

/// Semantic: publish the latest draft immutably (+ audit + FTS reindex).
#[tauri::command]
async fn publish_semantics(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    source: String,
) -> Result<String, String> {
    let id = querora_contracts::SourceId::new(source);
    let row = state
        .store
        .latest_draft(&id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no draft to publish")?;
    let version = state
        .store
        .publish(row.id, &row.graph)
        .await
        .map_err(|e| e.to_string())?;
    let graph = state
        .store
        .published_graph(&id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("publish lost")?;
    querora_core::semantic::index_graph(&state.store, &graph)
        .await
        .map_err(|e| e.message)?;
    // swap the served graph so tools answer from the published version
    state.set_semantic(graph);
    let _ = state
        .store
        .audit("ui", "publish_semantics", &format!("{id} → {version}"))
        .await;
    Ok(version)
}

/// Semantic: AI enrichment via the connected claude agent (fallback-aware).
/// Streams `AgentEvent`s to `agent-event://semantic-enrich` so the UI can
/// show live progress (tokens + tool calls + done).
#[tauri::command]
async fn enrich_semantics(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    source: String,
) -> Result<serde_json::Value, String> {
    use tauri::Emitter;
    let id = querora_contracts::SourceId::new(source);
    let draft = state
        .store
        .latest_draft(&id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("generate a draft first")?
        .graph;
    let socket = querora_core::toolapi::default_run_dir().join("querora.sock");
    let token = querora_core::toolapi::get_or_create_token(state.creds.as_ref())
        .map_err(|e| e.to_string())?;
    let _ = app.emit(
        "agent-event://semantic-enrich",
        &serde_json::json!({ "type": "status", "text": "asking claude for label/description/alias patches (bounded subset)…" }),
    );
    match querora_core::semantic::enrich(
        &state.store,
        &draft,
        querora_core::toolapi::default_run_dir(),
        socket,
        &token,
    )
    .await
    {
        Ok(querora_core::semantic::EnrichResult::Enriched(g)) => {
            state
                .store
                .save_draft(&id, &g)
                .await
                .map_err(|e| e.to_string())?;
            querora_core::semantic::index_graph(&state.store, &g)
                .await
                .map_err(|e| e.message)?;
            let _ = app.emit(
                "agent-event://semantic-enrich",
                &serde_json::json!({ "type": "done" }),
            );
            Ok(serde_json::json!({ "status": "enriched", "graph": g }))
        }
        Ok(querora_core::semantic::EnrichResult::FellBack(reason)) => {
            let _ = app.emit(
                "agent-event://semantic-enrich",
                &serde_json::json!({ "type": "done" }),
            );
            Ok(serde_json::json!({ "status": "fell_back", "reason": reason, "graph": draft }))
        }
        Err(e) => {
            let _ = app.emit(
                "agent-event://semantic-enrich",
                &serde_json::json!({ "type": "done" }),
            );
            Ok(
                serde_json::json!({ "status": "fell_back", "reason": e.to_string(), "graph": draft }),
            )
        }
    }
}

/// Schema explorer: cached catalog + drift vs it on re-introspect.
#[tauri::command]
async fn introspect(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    source: String,
) -> Result<serde_json::Value, String> {
    let id = querora_contracts::SourceId::new(source);
    let ds = state
        .sources
        .get(&id, &state.store, state.creds.as_ref())
        .await
        .map_err(|e| e.message)?;
    let fresh = ds.catalog().await.map_err(|e| e.message)?;
    let old = state
        .store
        .cached_catalog(&id)
        .await
        .map_err(|e| e.to_string())?;
    let drift = old
        .as_ref()
        .map(|o| querora_core::connectors::drift::diff_catalogs(o, &fresh));
    state
        .store
        .set_catalog(&id, &fresh)
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "catalog": fresh, "drift": drift }))
}

/// Dual mode: enable (rotate token) + registration snippets for consent.
#[tauri::command]
async fn dualmode_enable(
    server: tauri::State<'_, Arc<querora_core::toolapi::ToolApiServer>>,
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
) -> Result<serde_json::Value, String> {
    let token = querora_core::dualmode::rotate_token().map_err(|e| e.message)?;
    server.inner().set_dual_token(Some(token.clone()));
    let _ = state
        .store
        .audit(
            "dualmode",
            "enable",
            "terminal access enabled (token rotated)",
        )
        .await;
    let shim = querora_core::agents::mcp_shim_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "querora-mcp (build crates/mcp)".to_string());
    Ok(serde_json::json!({
        "token_file": querora_core::dualmode::token_file().display().to_string(),
        "claude": format!("claude mcp add querora --env QUERORA_DUAL_TOKEN={token} -- {shim}"),
        "codex": format!("[mcp_servers.querora]\ncommand = \"{shim}\"\nargs = []\nenv = {{ QUERORA_DUAL_TOKEN = \"{token}\" }}\nrequires_approval = false"),
    }))
}

/// Dual mode: disable (remove token file + in-memory).
#[tauri::command]
async fn dualmode_disable(
    server: tauri::State<'_, Arc<querora_core::toolapi::ToolApiServer>>,
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
) -> Result<(), String> {
    std::fs::remove_file(querora_core::dualmode::token_file()).ok();
    server.inner().set_dual_token(None);
    let _ = state
        .store
        .audit("dualmode", "disable", "terminal access disabled")
        .await;
    Ok(())
}

/// Dual mode: recent external connections (audit view).
#[tauri::command]
async fn dualmode_connections(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
) -> Result<Vec<serde_json::Value>, String> {
    let entries = state
        .store
        .audit_entries(50)
        .await
        .map_err(|e| e.to_string())?;
    Ok(entries
        .into_iter()
        .filter(|(_, actor, _, _)| actor == "dualmode")
        .map(|(ts, actor, tool, summary)| serde_json::json!({ "ts": ts, "actor": actor, "tool": tool, "summary": summary }))
        .collect())
}

/// Agent preference shape persisted in app_settings.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentPrefs {
    /// Default agent for new chats: claude | codex | pi.
    pub default_agent: String,
    /// pi model id (e.g. `zai/glm-5.3`).
    pub pi_model: String,
    /// pi thinking effort: off|minimal|low|medium|high|xhigh|max.
    pub pi_effort: String,
    /// claude model override (empty = claude default).
    pub claude_model: String,
}

impl Default for AgentPrefs {
    fn default() -> Self {
        Self {
            default_agent: "claude".into(),
            pi_model: "zai/glm-5.3".into(),
            pi_effort: "medium".into(),
            claude_model: String::new(),
        }
    }
}

const PREFS_KEY: &str = "agent_prefs";

/// Read agent preferences (defaults: agent=pi, model=zai/glm-5.3, effort=medium).
#[tauri::command]
async fn get_agent_prefs(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
) -> Result<AgentPrefs, String> {
    match state
        .store
        .get_setting(PREFS_KEY)
        .await
        .map_err(|e| e.to_string())?
    {
        Some(raw) => serde_json::from_str(&raw).map_err(|e| e.to_string()),
        None => Ok(AgentPrefs::default()),
    }
}

/// Save agent preferences.
#[tauri::command]
async fn set_agent_prefs(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    prefs: AgentPrefs,
) -> Result<(), String> {
    let raw = serde_json::to_string(&prefs).map_err(|e| e.to_string())?;
    state
        .store
        .set_setting(PREFS_KEY, &raw)
        .await
        .map_err(|e| e.to_string())
}

/// Fetch chat history for a session (UI hydration after tab switches).
#[tauri::command]
async fn session_messages(
    state: tauri::State<'_, Arc<querora_core::toolapi::ToolContext>>,
    session_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let rows = state
        .store
        .messages(&session_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|m| serde_json::json!({ "role": m.role, "content": m.content, "created_at": m.created_at }))
        .collect())
}

/// Mounts the Querora window. Run by `main.rs` (desktop) and the mobile
/// entry point (not an M0 target).
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            mount_core(app.handle())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ping,
            querora_status,
            probe_agents,
            list_sessions,
            chat_send,
            append_user_message,
            get_result,
            export_csv,
            list_sources,
            add_source,
            test_source,
            remove_source,
            draft_semantics,
            latest_draft,
            published_graph,
            publish_semantics,
            enrich_semantics,
            introspect,
            dualmode_enable,
            dualmode_disable,
            dualmode_connections,
            session_messages,
            get_agent_prefs,
            set_agent_prefs,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
