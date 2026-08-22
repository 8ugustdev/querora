/* SPDX-License-Identifier: Apache-2.0 */
import type { AgentEvent, AgentStatus } from "./contracts";
import { invoke } from "@tauri-apps/api/core";

export interface SessionDto {
  id: string;
  agent: string;
  title: string;
  createdAt: string;
  updatedAt: string;
}

export function ping(): Promise<string> {
  return invoke<string>("ping");
}

export interface CoreStatus {
  socket_path: string;
  semantic_version: string | null;
  source: string | null;
}

export function queroraStatus(): Promise<CoreStatus> {
  return invoke<CoreStatus>("querora_status");
}

export function probeAgents(): Promise<AgentStatus[]> {
  return invoke<AgentStatus[]>("probe_agents");
}

export function listSessions(): Promise<SessionDto[]> {
  return invoke<SessionDto[]>("list_sessions");
}

export interface HistoryMessage {
  role: string;
  content: { text?: string; tool_timeline?: Array<{ kind: string; tool: string; ok?: boolean; summary?: string }> };
  created_at: string;
}

export function sessionMessages(sessionId: string): Promise<HistoryMessage[]> {
  return invoke("session_messages", { sessionId });
}

export interface AgentPrefs {
  defaultAgent: string;
  piModel: string;
  piEffort: string;
  claudeModel: string;
}

export function getAgentPrefs(): Promise<AgentPrefs> {
  return invoke("get_agent_prefs");
}

export function setAgentPrefs(prefs: AgentPrefs): Promise<void> {
  return invoke("set_agent_prefs", { prefs });
}

export function chatSend(sessionId: string, agent: string, prompt: string): Promise<{ ok: boolean }> {
  return invoke("chat_send", { sessionId, agent, prompt });
}

export function listSources(): Promise<import("./contracts").SourceInfo[]> {
  return invoke("list_sources");
}

export function addSource(
  info: import("./contracts").SourceInfo,
  secret: string | undefined,
): Promise<void> {
  return invoke("add_source", { info, secret: secret ?? null });
}

export function testSource(
  info: import("./contracts").SourceInfo,
  secret: string | undefined,
): Promise<{ ok: boolean; tables: number; dialect: string }> {
  return invoke("test_source", { info, secret: secret ?? null });
}

export function removeSource(id: string): Promise<void> {
  return invoke("remove_source", { id });
}

export function getResult(resultId: string): Promise<import("./contracts").QueryResult | null> {
  return invoke("get_result", { resultId });
}

export function exportCsv(resultId: string): Promise<string> {
  return invoke("export_csv", { resultId });
}

export interface DraftResult {
  graph: import("./contracts").SemanticGraph;
  unjoined_tables: string[];
  candidate_relationships: string[];
}

export function draftSemantics(source: string): Promise<DraftResult> {
  return invoke("draft_semantics", { source });
}

export function enrichSemantics(source: string): Promise<{ status: string; reason?: string; graph: import("./contracts").SemanticGraph }> {
  return invoke("enrich_semantics", { source });
}

export function publishSemantics(source: string): Promise<string> {
  return invoke("publish_semantics", { source });
}

export interface IntrospectResult {
  catalog: {
    tables: Array<{
      name: string;
      is_view: boolean;
      columns: Array<{ name: string; data_type: string; nullable: boolean; primary_key: boolean }>;
    }>;
  };
  drift: { entries: Array<{ type: string; value: { table?: string; column?: string } }> } | null;
}

export function introspect(source: string): Promise<IntrospectResult> {
  return invoke("introspect", { source });
}

export interface DualModeInfo {
  token_file: string;
  claude: string;
  codex: string;
}

export function dualmodeEnable(): Promise<DualModeInfo> {
  return invoke("dualmode_enable");
}

export function dualmodeDisable(): Promise<void> {
  return invoke("dualmode_disable");
}

export function dualmodeConnections(): Promise<Array<{ ts: string; actor: string; tool: string; summary: string }>> {
  return invoke("dualmode_connections");
}

/** Subscribe to streamed agent events for a session. Returns unsubscribe. */
export function onAgentEvent(
  sessionId: string,
  handler: (ev: AgentEvent) => void,
): () => void {
  let unlisten: (() => void) | undefined;
  let cancelled = false;
  import("@tauri-apps/api/event").then(({ listen }) => {
    if (cancelled) return;
    listen(`agent-event://${sessionId}`, (e) => handler(e.payload as AgentEvent)).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
  });
  return () => {
    cancelled = true;
    unlisten?.();
  };
}
